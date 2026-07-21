//! Owner-scoped campaign lifecycle, history, and private export storage.
//!
//! The current product has one explicit local owner. Every query still takes
//! an owner key so hosted identity can replace the local key without adding an
//! unscoped data path. Nothing in this module enables hosted access.

use std::collections::BTreeSet;

use manchester_dnd_core::{
    CAMPAIGN_PINS_SCHEMA_VERSION, CampaignContentPins, CampaignPinSealReason, Character,
    SESSION_SCHEMA_VERSION, SealedCampaignPins, SessionDto, SessionEventDto, Sha256Digest,
    encounter::{EncounterCommand, EncounterIntent},
    hero::HeroPins,
    hero::{
        HERO_CHARACTER_SCHEMA_VERSION, HERO_DRAFT_SCHEMA_VERSION, HeroCharacter, HeroCreationDraft,
    },
    is_valid_opaque_id,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::{Postgres, Row, Transaction, postgres::PgRow};

use super::{
    CHARACTER_SCHEMA_VERSION, GeneratedAssetMetadata, PostgresRepository, from_i64, from_i64_u32,
    recaps::{CampaignPrivateRecap, private_recap_from_row},
    serialize, to_i64,
};
use crate::{error::RepositoryError, repository::HeroAuditPayload};

pub const CAMPAIGN_LIFECYCLE_SCHEMA_VERSION: u16 = 1;
pub const CAMPAIGN_EXPORT_SCHEMA_VERSION: u16 = 1;
pub const CAMPAIGN_HISTORY_DEFAULT_LIMIT: u16 = 25;
pub const CAMPAIGN_HISTORY_MAX_LIMIT: u16 = 100;
const MAX_PLAYER_EXPORT_BYTES: usize = 2 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CampaignLifecycleState {
    Active,
    Archived,
}

impl CampaignLifecycleState {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Archived => "archived",
        }
    }
}

impl TryFrom<&str> for CampaignLifecycleState {
    type Error = RepositoryError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "active" => Ok(Self::Active),
            "archived" => Ok(Self::Archived),
            _ => invalid("campaign lifecycle", value, "unknown lifecycle state"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CampaignSummary {
    pub schema_version: u16,
    pub campaign_session_id: String,
    pub owner_key: String,
    pub title: String,
    pub campaign_revision: u64,
    pub lifecycle_revision: u64,
    pub lifecycle_state: CampaignLifecycleState,
    pub archived_at: Option<String>,
    pub safety_policy_id: String,
    pub progression_policy_id: String,
    pub retention_class: String,
    pub retention_delete_after: Option<String>,
    pub open_play_session_id: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CampaignLifecycleCommand {
    pub schema_version: u16,
    pub campaign_session_id: String,
    pub expected_lifecycle_revision: u64,
    pub idempotency_key: String,
}

impl CampaignLifecycleCommand {
    pub fn validate(&self) -> Result<(), RepositoryError> {
        if self.schema_version != CAMPAIGN_LIFECYCLE_SCHEMA_VERSION {
            return Err(RepositoryError::UnsupportedSchemaVersion {
                entity: "campaign lifecycle command",
                found: u32::from(self.schema_version),
                supported: u32::from(CAMPAIGN_LIFECYCLE_SCHEMA_VERSION),
            });
        }
        if !is_valid_opaque_id(&self.campaign_session_id)
            || !is_valid_opaque_id(&self.idempotency_key)
            || self.expected_lifecycle_revision == 0
        {
            return invalid(
                "campaign lifecycle command",
                &self.campaign_session_id,
                "ids and expected lifecycle revision must be valid",
            );
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StartPlaySessionCommand {
    pub lifecycle: CampaignLifecycleCommand,
    pub play_session_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EndPlaySessionCommand {
    pub lifecycle: CampaignLifecycleCommand,
    pub play_session_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeleteCampaignCommand {
    pub lifecycle: CampaignLifecycleCommand,
    pub deletion_id: String,
    pub confirm_permanent_delete: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedCampaignDeletion {
    pub schema_version: u16,
    pub campaign_session_id: String,
    pub deletion_id: String,
    pub campaign_revision: u64,
    pub lifecycle_revision: u64,
    pub canonical_export_digest: Sha256Digest,
    pub canonical_export_json: String,
    pub expires_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CampaignLifecycleOutcome {
    pub schema_version: u16,
    pub campaign_session_id: String,
    pub lifecycle_revision: u64,
    pub lifecycle_state: Option<CampaignLifecycleState>,
    pub play_session_id: Option<String>,
    pub deleted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CampaignPlaySession {
    pub schema_version: u16,
    pub id: String,
    pub campaign_session_id: String,
    pub owner_key: String,
    pub state: String,
    pub started_campaign_revision: u64,
    pub ended_campaign_revision: Option<u64>,
    pub opened_at: String,
    pub closed_at: Option<String>,
    pub close_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CampaignTurnHistoryItem {
    pub schema_version: u32,
    pub id: String,
    pub campaign_session_id: String,
    pub turn_number: u64,
    pub actor_id: Option<String>,
    pub correlation_id: Option<String>,
    pub event: SessionEventDto,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CampaignTurnHistoryPage {
    pub schema_version: u16,
    pub campaign_session_id: String,
    pub items: Vec<CampaignTurnHistoryItem>,
    pub next_after_turn_number: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum LifecycleAuditPayload {
    PlayStarted {
        play_session_id: String,
    },
    PlayEnded {
        play_session_id: String,
    },
    Archived,
    Restored,
    RestoreImported {
        closed_play_session_ids: Vec<String>,
    },
}

impl LifecycleAuditPayload {
    const fn event_kind(&self) -> &'static str {
        match self {
            Self::PlayStarted { .. } => "play_started",
            Self::PlayEnded { .. } => "play_ended",
            Self::Archived => "archived",
            Self::Restored => "restored",
            Self::RestoreImported { .. } => "restore_imported",
        }
    }

    fn validate(&self) -> bool {
        match self {
            Self::PlayStarted { play_session_id } | Self::PlayEnded { play_session_id } => {
                is_valid_opaque_id(play_session_id)
            }
            Self::Archived | Self::Restored => true,
            Self::RestoreImported {
                closed_play_session_ids,
            } => {
                closed_play_session_ids.len() <= 64
                    && closed_play_session_ids
                        .iter()
                        .all(|id| is_valid_opaque_id(id))
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExportedDocument<T> {
    pub id: String,
    pub schema_version: u32,
    pub revision: u64,
    pub value: T,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExportedCampaignDocument {
    pub document: ExportedDocument<SessionDto>,
    pub owner_key: String,
    pub lifecycle_revision: u64,
    pub lifecycle_state: CampaignLifecycleState,
    pub archived_at: Option<String>,
    pub safety_policy_id: String,
    pub progression_policy_id: String,
    pub retention_class: String,
    pub retention_delete_after: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExportedCampaignPins {
    pub evidence: SealedCampaignPins,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExportedTurnAudit {
    pub id: String,
    pub turn_number: u64,
    pub actor_id: Option<String>,
    pub correlation_id: Option<String>,
    pub schema_version: u32,
    pub event: SessionEventDto,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExportedCommandReceipt {
    pub idempotency_key: String,
    pub command_kind: String,
    pub request_fingerprint: Sha256Digest,
    pub expected_revision: u64,
    pub result_revision: u64,
    pub audit_id: String,
    pub response: Value,
    pub created_at: String,
}

/// Body-free, campaign-lifetime alias for a generated narration response.
/// Presentation bodies and operational attempts can expire independently.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExportedTextPresentationReceipt {
    pub schema_version: u16,
    pub origin_turn_id: String,
    pub client_idempotency_key: String,
    pub presentation_id: String,
    pub generation_job_id: String,
    pub generation_attempt_id: String,
    pub version: u8,
    pub source: String,
    pub config_digest: Sha256Digest,
    pub prompt_digest: Sha256Digest,
    pub policy_digest: Sha256Digest,
    pub output_digest: Sha256Digest,
    pub created_at: String,
}

/// Mechanics-resumption receipt for a validated free-form command. The raw
/// player text is represented only by its digest and is never exported.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExportedTypedIntentReceipt {
    pub schema_version: u16,
    pub client_idempotency_key: String,
    pub player_intent_digest: Sha256Digest,
    pub expected_campaign_revision: u64,
    pub expected_encounter_revision: u64,
    pub resolved_intent: EncounterIntent,
    pub interpretation_label: String,
    pub interpretation_evidence: Value,
    pub state: String,
    pub origin_turn_id: Option<String>,
    pub event_sequence: Option<u64>,
    pub result_campaign_revision: Option<u64>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExportedHeroDraft {
    pub document: ExportedDocument<HeroCreationDraft>,
    pub expires_at_epoch_seconds: u64,
    pub retention_delete_after_epoch_seconds: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExportedHeroAudit {
    pub id: String,
    pub subject_kind: String,
    pub subject_id: String,
    pub audit_kind: String,
    pub schema_version: u32,
    pub subject_revision: u64,
    pub occurred_at_epoch_seconds: u64,
    pub payload: HeroAuditPayload,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExportedHeroReceipt {
    pub scope_kind: String,
    pub scope_id: String,
    pub idempotency_key: String,
    pub command_kind: String,
    pub request_fingerprint: Sha256Digest,
    pub expected_revision: u64,
    pub result_revision: u64,
    pub audit_id: String,
    pub response: Value,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExportedEncounterRewardClaim {
    pub encounter_id: String,
    pub character_id: String,
    pub encounter_revision: u64,
    pub victory_event_sequence: u64,
    pub reward_tier: String,
    pub experience_awarded: u32,
    pub hero_audit_id: String,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExportedLifecycleAudit {
    pub id: String,
    pub lifecycle_revision: u64,
    pub payload: LifecycleAuditPayload,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExportedTextPresentation {
    pub id: String,
    pub origin_turn_id: String,
    pub generation_job_id: String,
    pub generation_attempt_id: String,
    pub client_idempotency_key: String,
    pub version: u8,
    pub source: String,
    pub body: String,
    pub config_digest: Sha256Digest,
    pub prompt_digest: Sha256Digest,
    pub policy_digest: Sha256Digest,
    pub output_digest: Sha256Digest,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExportedGeneratedAsset {
    pub id: String,
    pub turn_id: Option<String>,
    pub asset_kind: String,
    pub provider: String,
    pub model: String,
    pub location: String,
    pub prompt_fingerprint: Option<Sha256Digest>,
    pub metadata: GeneratedAssetMetadata,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CampaignPrivateExportV1 {
    pub schema_version: u16,
    pub owner_key: String,
    pub exported_at: String,
    pub campaign: ExportedCampaignDocument,
    pub characters: Vec<ExportedDocument<Character>>,
    pub hero_drafts: Vec<ExportedHeroDraft>,
    pub hero_character: Option<ExportedDocument<HeroCharacter>>,
    pub turns: Vec<ExportedTurnAudit>,
    pub command_receipts: Vec<ExportedCommandReceipt>,
    pub text_presentation_receipts: Vec<ExportedTextPresentationReceipt>,
    pub typed_intent_receipts: Vec<ExportedTypedIntentReceipt>,
    pub hero_audits: Vec<ExportedHeroAudit>,
    pub hero_receipts: Vec<ExportedHeroReceipt>,
    pub encounter_reward_claims: Vec<ExportedEncounterRewardClaim>,
    pub content_pins: Option<ExportedCampaignPins>,
    pub play_sessions: Vec<CampaignPlaySession>,
    pub lifecycle_audits: Vec<ExportedLifecycleAudit>,
    #[serde(default)]
    pub private_recaps: Vec<CampaignPrivateRecap>,
    pub selected_text_presentations: Vec<ExportedTextPresentation>,
    pub selected_generated_assets: Vec<ExportedGeneratedAsset>,
}

impl CampaignPrivateExportV1 {
    pub fn validate(&self) -> Result<(), RepositoryError> {
        validate_export(self)
    }

    pub fn canonical_json(&self) -> Result<String, RepositoryError> {
        self.validate()?;
        canonical_json(self)
    }

    pub fn canonical_digest(&self) -> Result<Sha256Digest, RepositoryError> {
        Ok(digest(self.canonical_json()?.as_bytes()))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RestoreCampaignExportCommand {
    pub schema_version: u16,
    pub idempotency_key: String,
    pub canonical_export_json: String,
}

impl PostgresRepository {
    pub async fn list_owned_campaigns(
        &self,
        owner_key: &str,
    ) -> Result<Vec<CampaignSummary>, RepositoryError> {
        validate_owner_key(owner_key)?;
        let rows = sqlx::query(
            "SELECT c.id, c.owner_key, c.revision, c.lifecycle_revision, c.lifecycle_state,
                    c.archived_at::text AS archived_at, c.safety_policy_id,
                    c.progression_policy_id, c.retention_class,
                    c.retention_delete_after::text AS retention_delete_after,
                    c.payload_json->>'title' AS title,
                    c.created_at::text AS created_at, c.updated_at::text AS updated_at,
                    p.id AS open_play_session_id
             FROM campaign_sessions c
             LEFT JOIN campaign_play_sessions p
               ON p.campaign_session_id = c.id AND p.state IN ('waiting', 'active')
             WHERE c.owner_key = $1
             ORDER BY c.updated_at DESC, c.id",
        )
        .bind(owner_key)
        .fetch_all(&self.pool)
        .await
        .map_err(RepositoryError::Database)?;
        rows.into_iter().map(summary_from_row).collect()
    }

    pub async fn load_owned_campaign_summary(
        &self,
        owner_key: &str,
        campaign_session_id: &str,
    ) -> Result<Option<CampaignSummary>, RepositoryError> {
        validate_owner_campaign(owner_key, campaign_session_id)?;
        let row = sqlx::query(
            "SELECT c.id, c.owner_key, c.revision, c.lifecycle_revision, c.lifecycle_state,
                    c.archived_at::text AS archived_at, c.safety_policy_id,
                    c.progression_policy_id, c.retention_class,
                    c.retention_delete_after::text AS retention_delete_after,
                    c.payload_json->>'title' AS title,
                    c.created_at::text AS created_at, c.updated_at::text AS updated_at,
                    p.id AS open_play_session_id
             FROM campaign_sessions c
             LEFT JOIN campaign_play_sessions p
               ON p.campaign_session_id = c.id AND p.state IN ('waiting', 'active')
             WHERE c.owner_key = $1 AND c.id = $2",
        )
        .bind(owner_key)
        .bind(campaign_session_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(RepositoryError::Database)?;
        row.map(summary_from_row).transpose()
    }

    pub async fn has_campaign_deletion_tombstone(
        &self,
        owner_key: &str,
        campaign_session_id: &str,
    ) -> Result<bool, RepositoryError> {
        validate_owner_campaign(owner_key, campaign_session_id)?;
        sqlx::query_scalar(
            "SELECT EXISTS(
                 SELECT 1 FROM campaign_deletion_tombstones
                 WHERE owner_key = $1 AND campaign_session_id = $2
                   AND retention_delete_after > CURRENT_TIMESTAMP
             )",
        )
        .bind(owner_key)
        .bind(campaign_session_id)
        .fetch_one(&self.pool)
        .await
        .map_err(RepositoryError::Database)
    }

    pub(crate) async fn retire_deleted_campaign_receipts_for_recreate(
        &self,
        owner_key: &str,
        campaign_session_id: &str,
    ) -> Result<u64, RepositoryError> {
        validate_owner_campaign(owner_key, campaign_session_id)?;
        let mut transaction = self.pool.begin().await.map_err(RepositoryError::Database)?;
        let campaign_exists: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM campaign_sessions WHERE id = $1)")
                .bind(campaign_session_id)
                .fetch_one(&mut *transaction)
                .await
                .map_err(RepositoryError::Database)?;
        let retained_delete: bool = sqlx::query_scalar(
            "SELECT EXISTS(
                 SELECT 1 FROM campaign_deletion_tombstones
                 WHERE owner_key = $1 AND campaign_session_id = $2
                   AND retention_delete_after > CURRENT_TIMESTAMP
             )",
        )
        .bind(owner_key)
        .bind(campaign_session_id)
        .fetch_one(&mut *transaction)
        .await
        .map_err(RepositoryError::Database)?;
        if campaign_exists || !retained_delete {
            transaction
                .commit()
                .await
                .map_err(RepositoryError::Database)?;
            return Ok(0);
        }
        let deleted = sqlx::query(
            "DELETE FROM campaign_lifecycle_receipts
             WHERE owner_key = $1 AND campaign_session_id = $2",
        )
        .bind(owner_key)
        .bind(campaign_session_id)
        .execute(&mut *transaction)
        .await
        .map_err(RepositoryError::Database)?
        .rows_affected();
        transaction
            .commit()
            .await
            .map_err(RepositoryError::Database)?;
        Ok(deleted)
    }

    pub async fn list_campaign_turn_history(
        &self,
        owner_key: &str,
        campaign_session_id: &str,
        after_turn_number: Option<u64>,
        limit: u16,
    ) -> Result<CampaignTurnHistoryPage, RepositoryError> {
        validate_owner_campaign(owner_key, campaign_session_id)?;
        if limit == 0 || limit > CAMPAIGN_HISTORY_MAX_LIMIT {
            return invalid(
                "campaign history request",
                campaign_session_id,
                "page limit must be between one and one hundred",
            );
        }
        require_owned_campaign(&self.pool, owner_key, campaign_session_id).await?;
        let cursor = after_turn_number
            .map(|value| to_i64(value, "turn history cursor"))
            .transpose()?;
        let rows = sqlx::query(
            "SELECT id, campaign_session_id, turn_number, actor_id, correlation_id,
                    schema_version, payload_json::text AS payload_json,
                    created_at::text AS created_at
             FROM turn_audits
             WHERE campaign_session_id = $1
               AND ($2::bigint IS NULL OR turn_number > $2)
             ORDER BY turn_number, id
             LIMIT $3",
        )
        .bind(campaign_session_id)
        .bind(cursor)
        .bind(i64::from(limit) + 1)
        .fetch_all(&self.pool)
        .await
        .map_err(RepositoryError::Database)?;
        let has_more = rows.len() > usize::from(limit);
        let mut items = rows
            .into_iter()
            .take(usize::from(limit))
            .map(turn_history_from_row)
            .collect::<Result<Vec<_>, _>>()?;
        let next_after_turn_number = has_more
            .then(|| items.last().map(|item| item.turn_number))
            .flatten();
        if items
            .iter()
            .any(|item| item.campaign_session_id != campaign_session_id)
        {
            return invalid(
                "campaign history",
                campaign_session_id,
                "stored turn belongs to another campaign",
            );
        }
        Ok(CampaignTurnHistoryPage {
            schema_version: CAMPAIGN_LIFECYCLE_SCHEMA_VERSION,
            campaign_session_id: campaign_session_id.to_owned(),
            items: std::mem::take(&mut items),
            next_after_turn_number,
        })
    }

    pub async fn list_campaign_play_sessions(
        &self,
        owner_key: &str,
        campaign_session_id: &str,
    ) -> Result<Vec<CampaignPlaySession>, RepositoryError> {
        validate_owner_campaign(owner_key, campaign_session_id)?;
        require_owned_campaign(&self.pool, owner_key, campaign_session_id).await?;
        let rows = sqlx::query(
            "SELECT id, campaign_session_id, owner_key, schema_version, state,
                    started_campaign_revision, ended_campaign_revision,
                    opened_at::text AS opened_at, closed_at::text AS closed_at,
                    close_reason
             FROM campaign_play_sessions
             WHERE campaign_session_id = $1 AND owner_key = $2
             ORDER BY opened_at, id",
        )
        .bind(campaign_session_id)
        .bind(owner_key)
        .fetch_all(&self.pool)
        .await
        .map_err(RepositoryError::Database)?;
        rows.into_iter().map(play_session_from_row).collect()
    }
}

#[cfg(test)]
mod tests {
    use manchester_dnd_core::{
        AbilityScores, CampaignPinSealReason, CharacterDraft, D20Roll, EventActor, RULESET,
        RollMode, SESSION_SCHEMA_VERSION, SealedCampaignPins, SessionEventDto, SessionEventPayload,
        SessionStatus, hero::ThemeId,
    };
    use sqlx::PgPool;

    use super::*;
    use crate::{campaign_pins::CampaignPinRuntime, repository::MIGRATOR};

    const OWNER: &str = "local-owner";
    const CAMPAIGN: &str = "local-campaign";
    const CHARACTER: &str = "local-hero";

    fn repository(pool: PgPool) -> PostgresRepository {
        PostgresRepository::from_pool(pool)
    }

    fn session() -> SessionDto {
        SessionDto {
            schema_version: SESSION_SCHEMA_VERSION,
            id: CAMPAIGN.to_owned(),
            ruleset: RULESET,
            title: "The Runes Beneath the Viaduct".to_owned(),
            status: SessionStatus::Active,
            character_ids: vec![CHARACTER.to_owned()],
            created_at_unix_ms: 1,
            updated_at_unix_ms: 1,
            last_event_sequence: 0,
        }
    }

    fn character() -> Character {
        CharacterDraft {
            id: CHARACTER.to_owned(),
            name: "Mara".to_owned(),
            theme: "Canal Warden".to_owned(),
            ability_scores: AbilityScores::new(12, 14, 10, 16, 13, 8).unwrap(),
            experience_points: 0,
            current_hit_points: 8,
            maximum_hit_points: 8,
        }
        .build()
        .unwrap()
    }

    async fn create_campaign(repository: &PostgresRepository) {
        repository
            .create_campaign(&session(), &[character()])
            .await
            .expect("campaign fixture should save");
    }

    fn lifecycle_command(expected: u64, key: &str) -> CampaignLifecycleCommand {
        CampaignLifecycleCommand {
            schema_version: CAMPAIGN_LIFECYCLE_SCHEMA_VERSION,
            campaign_session_id: CAMPAIGN.to_owned(),
            expected_lifecycle_revision: expected,
            idempotency_key: key.to_owned(),
        }
    }

    async fn seal_pins(repository: &PostgresRepository) {
        let runtime = CampaignPinRuntime::bundled_for_tests();
        let evidence = SealedCampaignPins {
            seal_reason: CampaignPinSealReason::SelectedTheme,
            pins: runtime.pins_for_theme(ThemeId::RainboundBorough).unwrap(),
            legacy_source: None,
        };
        repository
            .seal_campaign_pins_for_test(CAMPAIGN, &evidence)
            .await
            .expect("pin fixture should seal");
    }

    async fn append_event(
        repository: &PostgresRepository,
        current: &mut SessionDto,
        payload: SessionEventPayload,
        actor: EventActor,
    ) {
        let expected_revision = current.last_event_sequence + 1;
        let sequence = current.last_event_sequence + 1;
        current.last_event_sequence = sequence;
        current.updated_at_unix_ms = sequence + 10;
        let event = SessionEventDto {
            schema_version: SESSION_SCHEMA_VERSION,
            session_id: CAMPAIGN.to_owned(),
            sequence,
            occurred_at_unix_ms: current.updated_at_unix_ms,
            actor,
            payload,
        };
        repository
            .commit_session_event(
                &format!("turn-{sequence}"),
                current,
                expected_revision,
                &event,
                &[],
            )
            .await
            .expect("turn fixture should commit");
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn owner_scoped_lifecycle_has_exact_replay_and_play_boundaries(pool: PgPool) {
        let repository = repository(pool.clone());
        create_campaign(&repository).await;

        assert_eq!(
            repository.list_owned_campaigns(OWNER).await.unwrap().len(),
            1
        );
        assert!(
            repository
                .load_owned_campaign_summary("another-owner", CAMPAIGN)
                .await
                .unwrap()
                .is_none()
        );

        let start = StartPlaySessionCommand {
            lifecycle: lifecycle_command(1, "start-1"),
            play_session_id: "play-1".to_owned(),
        };
        let started = repository
            .start_campaign_play_session(OWNER, &start)
            .await
            .unwrap();
        assert_eq!(started.lifecycle_revision, 2);
        assert_eq!(
            repository
                .start_campaign_play_session(OWNER, &start)
                .await
                .unwrap(),
            started
        );
        assert!(matches!(
            repository
                .start_campaign_play_session(
                    OWNER,
                    &StartPlaySessionCommand {
                        lifecycle: lifecycle_command(1, "stale-start"),
                        play_session_id: "play-2".to_owned(),
                    },
                )
                .await,
            Err(RepositoryError::RevisionConflict { .. })
        ));
        assert!(
            repository
                .archive_campaign(OWNER, &lifecycle_command(2, "archive-open"))
                .await
                .is_err(),
            "archive must require an explicit play-session end"
        );

        let ended = repository
            .end_campaign_play_session(
                OWNER,
                &EndPlaySessionCommand {
                    lifecycle: lifecycle_command(2, "end-1"),
                    play_session_id: "play-1".to_owned(),
                },
            )
            .await
            .unwrap();
        assert_eq!(ended.lifecycle_revision, 3);
        let archived = repository
            .archive_campaign(OWNER, &lifecycle_command(3, "archive-1"))
            .await
            .unwrap();
        assert_eq!(
            archived.lifecycle_state,
            Some(CampaignLifecycleState::Archived)
        );
        let restored = repository
            .restore_archived_campaign(OWNER, &lifecycle_command(4, "restore-1"))
            .await
            .unwrap();
        assert_eq!(restored.lifecycle_revision, 5);
        assert_eq!(
            restored.lifecycle_state,
            Some(CampaignLifecycleState::Active)
        );

        let cascade_rule: String = sqlx::query_scalar(
            "SELECT confdeltype::text FROM pg_constraint
             WHERE conname = 'characters_campaign_session_id_fkey'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(cascade_rule, "c", "0010 must replace the exact 0001 FK");
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn delete_requires_server_preparation_cascades_and_keeps_35_day_tombstone(pool: PgPool) {
        let repository = repository(pool.clone());
        create_campaign(&repository).await;
        repository
            .archive_campaign(OWNER, &lifecycle_command(1, "archive-delete"))
            .await
            .unwrap();
        let prepared = repository
            .prepare_campaign_deletion(OWNER, CAMPAIGN, 2, "delete-preparation")
            .await
            .unwrap();
        assert_eq!(
            prepared.canonical_export_digest,
            digest(prepared.canonical_export_json.as_bytes())
        );

        let forged = DeleteCampaignCommand {
            lifecycle: lifecycle_command(2, "forged-delete"),
            deletion_id: "forged-preparation".to_owned(),
            confirm_permanent_delete: true,
        };
        assert!(matches!(
            repository.delete_archived_campaign(OWNER, &forged).await,
            Err(RepositoryError::NotFound { .. })
        ));
        assert!(
            repository
                .load_campaign_session(CAMPAIGN)
                .await
                .unwrap()
                .is_some()
        );

        let delete = DeleteCampaignCommand {
            lifecycle: lifecycle_command(2, "delete-command"),
            deletion_id: prepared.deletion_id,
            confirm_permanent_delete: true,
        };
        let deleted = repository
            .delete_archived_campaign(OWNER, &delete)
            .await
            .unwrap();
        assert!(deleted.deleted);
        assert_eq!(deleted.lifecycle_revision, 3);
        assert_eq!(
            repository
                .delete_archived_campaign(OWNER, &delete)
                .await
                .unwrap(),
            deleted,
            "delete replay must resolve from the retained receipt"
        );
        assert!(
            repository
                .load_campaign_session(CAMPAIGN)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            repository
                .load_character(CHARACTER)
                .await
                .unwrap()
                .is_none()
        );

        let tombstone_json: Value = sqlx::query_scalar(
            "SELECT to_jsonb(t) FROM campaign_deletion_tombstones t
             WHERE owner_key = $1 AND campaign_session_id = $2",
        )
        .bind(OWNER)
        .bind(CAMPAIGN)
        .fetch_one(&pool)
        .await
        .unwrap();
        let keys = tombstone_json.as_object().unwrap();
        assert!(!keys.contains_key("canonical_export_json"));
        assert!(!keys.contains_key("title"));
        let retention_seconds: i64 = sqlx::query_scalar(
            "SELECT EXTRACT(EPOCH FROM (retention_delete_after - deleted_at))::bigint
             FROM campaign_deletion_tombstones
             WHERE owner_key = $1 AND campaign_session_id = $2",
        )
        .bind(OWNER)
        .bind(CAMPAIGN)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(retention_seconds, 35 * 24 * 60 * 60);

        sqlx::query(
            "UPDATE campaign_deletion_tombstones
             SET retention_delete_after = CURRENT_TIMESTAMP + INTERVAL '1 hour'",
        )
        .execute(&pool)
        .await
        .unwrap();
        assert_eq!(
            repository
                .delete_expired_campaign_lifecycle_metadata(10)
                .await
                .unwrap()
                .2,
            0
        );
        sqlx::query(
            "UPDATE campaign_deletion_tombstones
             SET retention_delete_after = CURRENT_TIMESTAMP - INTERVAL '1 second'",
        )
        .execute(&pool)
        .await
        .unwrap();
        assert_eq!(
            repository
                .delete_expired_campaign_lifecycle_metadata(10)
                .await
                .unwrap()
                .2,
            1
        );
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn canonical_export_over_64k_restores_rolls_pins_provenance_and_closes_open_play(
        pool: PgPool,
    ) {
        let repository = repository(pool.clone());
        create_campaign(&repository).await;
        seal_pins(&repository).await;
        let mut current = session();
        append_event(
            &repository,
            &mut current,
            SessionEventPayload::DiceResolved {
                purpose: "Read the viaduct runes".to_owned(),
                roll: D20Roll {
                    mode: RollMode::Normal,
                    first: 14,
                    second: None,
                    selected: 14,
                },
                modifier: 2,
                total: 16,
            },
            EventActor::System,
        )
        .await;
        for index in 2..=8 {
            append_event(
                &repository,
                &mut current,
                SessionEventPayload::GmNarration {
                    text: format!("Scene {index}: {}", "rain over old brick. ".repeat(520)),
                    image_prompt: None,
                    source_prompt_id: None,
                },
                EventActor::AiGameMaster,
            )
            .await;
        }
        repository
            .start_campaign_play_session(
                OWNER,
                &StartPlaySessionCommand {
                    lifecycle: lifecycle_command(1, "open-before-export"),
                    play_session_id: "play-before-export".to_owned(),
                },
            )
            .await
            .unwrap();
        let d = format!("sha256:{}", "a".repeat(64));
        sqlx::query(
            "INSERT INTO generated_text_presentations
             (id, campaign_session_id, origin_turn_id, generation_job_id,
              generation_attempt_id, client_idempotency_key, version, source, body,
              config_digest, prompt_digest, policy_digest, output_digest,
              selected, retention_delete_after)
             VALUES ('presentation-1', $1, 'turn-1', 'job-selected', 'attempt-selected',
                     'client-selected', 1, 'engine_authored', 'The brass rune answers the rain.',
                     $2, $2, $2, $2, TRUE, NULL)",
        )
        .bind(CAMPAIGN)
        .bind(&d)
        .execute(&pool)
        .await
        .unwrap();

        sqlx::query(
            "INSERT INTO generated_text_presentation_receipts
             (campaign_session_id, origin_turn_id, schema_version,
              client_idempotency_key, presentation_id, generation_job_id,
              generation_attempt_id, version, source, config_digest,
              prompt_digest, policy_digest, output_digest, created_at)
             VALUES ($1, 'turn-1', 1, 'client-selected', 'presentation-1',
                     'job-selected', 'attempt-selected', 1, 'engine_authored',
                     $2, $2, $2, $2,
                     (SELECT created_at FROM generated_text_presentations
                      WHERE id = 'presentation-1'))",
        )
        .bind(CAMPAIGN)
        .bind(&d)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO generated_text_presentation_receipts
             (campaign_session_id, origin_turn_id, schema_version,
              client_idempotency_key, presentation_id, generation_job_id,
              generation_attempt_id, version, source, config_digest,
              prompt_digest, policy_digest, output_digest)
             VALUES ($1, 'turn-1', 1, 'client-expired', 'presentation-expired',
                     'job-expired', 'attempt-expired', 2, 'provider',
                     $2, $2, $2, $2)",
        )
        .bind(CAMPAIGN)
        .bind(&d)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO typed_intent_command_receipts
             (campaign_session_id, client_idempotency_key, schema_version,
              player_intent_digest, expected_campaign_revision,
              expected_encounter_revision, resolved_intent_json,
              interpretation_label, interpretation_evidence_json, state,
              origin_turn_id, event_sequence, result_campaign_revision)
             VALUES ($1, 'typed-client-1', 1, $2, 1, 1,
                     '{\"type\":\"end_turn\"}'::jsonb, 'End the turn',
                     '{\"source\":\"authored_fallback\"}'::jsonb, 'committed',
                     'turn-1', 1, 2)",
        )
        .bind(CAMPAIGN)
        .bind(&d)
        .execute(&pool)
        .await
        .unwrap();

        let first_page = repository
            .list_campaign_turn_history(OWNER, CAMPAIGN, None, 3)
            .await
            .unwrap();
        assert_eq!(first_page.items.len(), 3);
        assert_eq!(first_page.next_after_turn_number, Some(3));
        let second_page = repository
            .list_campaign_turn_history(OWNER, CAMPAIGN, first_page.next_after_turn_number, 100)
            .await
            .unwrap();
        assert_eq!(second_page.items.len(), 5);

        let recap = repository
            .generate_private_recap(
                OWNER,
                &crate::repository::GeneratePrivateRecapCommand {
                    schema_version: crate::repository::PRIVATE_RECAP_SCHEMA_VERSION,
                    campaign_session_id: CAMPAIGN.to_owned(),
                    expected_campaign_revision: current.last_event_sequence + 1,
                    idempotency_key: "large-export-private-recap".to_owned(),
                },
            )
            .await
            .unwrap();
        assert_eq!(recap.source_audit_count, 8);

        let canonical = repository
            .export_campaign_canonical_json(OWNER, CAMPAIGN)
            .await
            .unwrap();
        assert!(canonical.len() > 64 * 1024);
        assert!(!canonical.contains("raw_private_source"));
        assert!(!canonical.contains("provider_response"));
        assert!(!canonical.contains("kick the secret red door"));
        assert!(!canonical.contains("expired superseded prose"));
        let canonical_value: Value = serde_json::from_str(&canonical).unwrap();
        let alias_values = canonical_value["text_presentation_receipts"]
            .as_array()
            .unwrap();
        assert_eq!(alias_values.len(), 2);
        assert!(alias_values.iter().all(|value| value.get("body").is_none()));
        assert!(canonical.contains("presentation-expired"));
        assert!(canonical.contains("private-recap-v1"));
        assert!(
            canonical_value["typed_intent_receipts"][0]
                .get("player_intent")
                .is_none()
        );
        let mut invalid_receipt_schema: CampaignPrivateExportV1 =
            serde_json::from_str(&canonical).unwrap();
        invalid_receipt_schema.text_presentation_receipts[0].schema_version = 2;
        assert!(invalid_receipt_schema.validate().is_err());
        let readable = repository
            .export_campaign_player_readable(OWNER, CAMPAIGN)
            .await
            .unwrap();
        assert!(readable.contains("Stored dice and rule facts"));
        assert!(readable.contains("Durable private recap"));
        assert!(!readable.contains("client-selected"));
        assert!(readable.contains("CC BY 4.0"));

        sqlx::query("DELETE FROM campaign_sessions WHERE id = $1")
            .bind(CAMPAIGN)
            .execute(&pool)
            .await
            .unwrap();
        let restored = repository
            .restore_campaign_export(
                OWNER,
                &RestoreCampaignExportCommand {
                    schema_version: CAMPAIGN_EXPORT_SCHEMA_VERSION,
                    idempotency_key: "restore-large-export".to_owned(),
                    canonical_export_json: canonical,
                },
            )
            .await
            .unwrap();
        assert_eq!(restored.lifecycle_revision, 3);
        assert!(restored.play_session_id.is_none());
        let plays = repository
            .list_campaign_play_sessions(OWNER, CAMPAIGN)
            .await
            .unwrap();
        assert_eq!(plays.len(), 1);
        assert_eq!(plays[0].state, "closed");
        assert_eq!(plays[0].close_reason.as_deref(), Some("restore_import"));
        let import_audit_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM campaign_lifecycle_audits
             WHERE campaign_session_id = $1 AND event_kind = 'restore_imported'",
        )
        .bind(CAMPAIGN)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(import_audit_count, 1);
        let client_key: String = sqlx::query_scalar(
            "SELECT client_idempotency_key FROM generated_text_presentations
             WHERE campaign_session_id = $1 AND selected",
        )
        .bind(CAMPAIGN)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(client_key, "client-selected");
        let restored_alias_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM generated_text_presentation_receipts
             WHERE campaign_session_id = $1 AND client_idempotency_key = 'client-selected'",
        )
        .bind(CAMPAIGN)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(restored_alias_count, 1);
        let restored_typed_state: String = sqlx::query_scalar(
            "SELECT state FROM typed_intent_command_receipts
             WHERE campaign_session_id = $1 AND client_idempotency_key = 'typed-client-1'",
        )
        .bind(CAMPAIGN)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(restored_typed_state, "committed");
        assert_eq!(
            repository
                .load_latest_private_recap(OWNER, CAMPAIGN)
                .await
                .unwrap()
                .unwrap()
                .body_digest,
            recap.body_digest
        );
        let retained_replay = repository
            .load_generated_text_presentation_replay(CAMPAIGN, "turn-1", "client-selected")
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            retained_replay,
            crate::repository::GeneratedTextPresentationReplay::Available(snapshot)
                if snapshot.requested.id == "presentation-1"
        ));
        let expired_replay = repository
            .load_generated_text_presentation_replay(CAMPAIGN, "turn-1", "client-expired")
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            expired_replay,
            crate::repository::GeneratedTextPresentationReplay::Expired { receipt, .. }
                if receipt.presentation_id == "presentation-expired"
        ));
        let typed_replay = repository
            .load_typed_intent_command_receipt(CAMPAIGN, "typed-client-1")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            typed_replay.state,
            crate::repository::TypedIntentReceiptState::Committed
        );
        assert_eq!(typed_replay.resolved_intent, EncounterIntent::EndTurn);
        assert_eq!(typed_replay.player_intent_digest.as_str(), d);
        assert_eq!(
            repository
                .list_campaign_turn_history(OWNER, CAMPAIGN, None, 100)
                .await
                .unwrap()
                .items
                .len(),
            8
        );
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn restore_rejects_unknown_schema_and_unsealed_playable_export(pool: PgPool) {
        let repository = repository(pool.clone());
        create_campaign(&repository).await;
        seal_pins(&repository).await;
        let canonical = repository
            .export_campaign_canonical_json(OWNER, CAMPAIGN)
            .await
            .unwrap();
        sqlx::query("DELETE FROM campaign_sessions WHERE id = $1")
            .bind(CAMPAIGN)
            .execute(&pool)
            .await
            .unwrap();

        let mut unknown: Value = serde_json::from_str(&canonical).unwrap();
        unknown["schema_version"] = Value::from(99);
        let unknown = canonical_json(&unknown).unwrap();
        assert!(matches!(
            repository
                .restore_campaign_export(
                    OWNER,
                    &RestoreCampaignExportCommand {
                        schema_version: CAMPAIGN_EXPORT_SCHEMA_VERSION,
                        idempotency_key: "unknown-schema".to_owned(),
                        canonical_export_json: unknown,
                    },
                )
                .await,
            Err(RepositoryError::UnsupportedSchemaVersion { .. })
        ));

        let mut unsealed: CampaignPrivateExportV1 = serde_json::from_str(&canonical).unwrap();
        unsealed.content_pins = None;
        unsealed.turns.push(ExportedTurnAudit {
            id: "invalid-playable-turn".to_owned(),
            turn_number: 1,
            actor_id: None,
            correlation_id: None,
            schema_version: u32::from(SESSION_SCHEMA_VERSION),
            event: SessionEventDto {
                schema_version: SESSION_SCHEMA_VERSION,
                session_id: CAMPAIGN.to_owned(),
                sequence: 1,
                occurred_at_unix_ms: 2,
                actor: EventActor::System,
                payload: SessionEventPayload::SessionStarted,
            },
            created_at: unsealed.exported_at.clone(),
        });
        unsealed.campaign.document.value.last_event_sequence = 1;
        unsealed.campaign.document.revision = 2;
        assert!(unsealed.validate().is_err());
    }
}

impl PostgresRepository {
    pub async fn restore_campaign_export(
        &self,
        owner_key: &str,
        command: &RestoreCampaignExportCommand,
    ) -> Result<CampaignLifecycleOutcome, RepositoryError> {
        validate_owner_key(owner_key)?;
        if command.schema_version != CAMPAIGN_EXPORT_SCHEMA_VERSION {
            return Err(RepositoryError::UnsupportedSchemaVersion {
                entity: "campaign restore command",
                found: u32::from(command.schema_version),
                supported: u32::from(CAMPAIGN_EXPORT_SCHEMA_VERSION),
            });
        }
        if !is_valid_opaque_id(&command.idempotency_key)
            || command.canonical_export_json.is_empty()
            || command.canonical_export_json.len() > MAX_PLAYER_EXPORT_BYTES
        {
            return invalid(
                "campaign restore command",
                &command.idempotency_key,
                "idempotency key or export size is invalid",
            );
        }
        let exported: CampaignPrivateExportV1 =
            serde_json::from_str(&command.canonical_export_json).map_err(|source| {
                RepositoryError::InvalidStoredData {
                    entity: "campaign private export",
                    id: command.idempotency_key.clone(),
                    source,
                }
            })?;
        exported.validate()?;
        if exported.owner_key != owner_key {
            return Err(RepositoryError::NotFound {
                entity: "campaign private export",
                id: "owner-scoped-export".to_owned(),
            });
        }
        let canonical = exported.canonical_json()?;
        if canonical != command.canonical_export_json {
            return invalid(
                "campaign private export",
                &exported.campaign.document.id,
                "restore input must use the canonical JSON serialization",
            );
        }
        let export_digest = digest(canonical.as_bytes());
        let campaign_session_id = exported.campaign.document.id.clone();
        let expected_revision = exported.campaign.lifecycle_revision;
        let result_revision =
            expected_revision
                .checked_add(1)
                .ok_or(RepositoryError::NumericRange {
                    field: "restored lifecycle revision",
                })?;
        let lifecycle = CampaignLifecycleCommand {
            schema_version: CAMPAIGN_LIFECYCLE_SCHEMA_VERSION,
            campaign_session_id: campaign_session_id.clone(),
            expected_lifecycle_revision: expected_revision,
            idempotency_key: command.idempotency_key.clone(),
        };
        let request_fingerprint =
            digest(format!("restore_export\n{}", export_digest.as_str()).as_bytes());
        let outcome = CampaignLifecycleOutcome {
            schema_version: CAMPAIGN_LIFECYCLE_SCHEMA_VERSION,
            campaign_session_id: campaign_session_id.clone(),
            lifecycle_revision: result_revision,
            lifecycle_state: Some(exported.campaign.lifecycle_state),
            play_session_id: None,
            deleted: false,
        };

        let mut transaction = self.pool.begin().await.map_err(RepositoryError::Database)?;
        if let Some(replayed) = replay_lifecycle_receipt(
            &mut transaction,
            owner_key,
            &lifecycle,
            "restore_export",
            &request_fingerprint,
        )
        .await?
        {
            transaction
                .commit()
                .await
                .map_err(RepositoryError::Database)?;
            return Ok(replayed);
        }
        let existing: Option<String> =
            sqlx::query_scalar("SELECT id FROM campaign_sessions WHERE id = $1")
                .bind(&campaign_session_id)
                .fetch_optional(&mut *transaction)
                .await
                .map_err(RepositoryError::Database)?;
        if existing.is_some() {
            return Err(RepositoryError::AlreadyExists {
                entity: "campaign session",
                id: campaign_session_id,
            });
        }

        insert_restored_campaign(&mut transaction, &exported, result_revision).await?;
        insert_restore_import_audit(&mut transaction, &exported, result_revision).await?;
        insert_lifecycle_receipt(
            &mut transaction,
            owner_key,
            &lifecycle,
            "restore_export",
            &request_fingerprint,
            &outcome,
        )
        .await?;
        transaction
            .commit()
            .await
            .map_err(RepositoryError::Database)?;
        Ok(outcome)
    }
}

async fn insert_restored_campaign(
    transaction: &mut Transaction<'_, Postgres>,
    exported: &CampaignPrivateExportV1,
    restored_lifecycle_revision: u64,
) -> Result<(), RepositoryError> {
    let campaign = &exported.campaign;
    let document = &campaign.document;
    sqlx::query(
        "INSERT INTO campaign_sessions
         (id, schema_version, revision, payload_json, created_at, updated_at,
          content_pin_legacy_eligible, owner_key, lifecycle_revision,
          lifecycle_state, archived_at, safety_policy_id, progression_policy_id,
          retention_class, retention_delete_after)
         VALUES ($1, $2, $3, $4::jsonb, $5::timestamptz, $6::timestamptz,
                 FALSE, $7, $8, $9, $10::timestamptz, $11, $12, $13,
                 $14::timestamptz)",
    )
    .bind(&document.id)
    .bind(i64::from(document.schema_version))
    .bind(to_i64(document.revision, "campaign revision")?)
    .bind(serialize("campaign session", &document.value)?)
    .bind(&document.created_at)
    .bind(&document.updated_at)
    .bind(&campaign.owner_key)
    .bind(to_i64(
        restored_lifecycle_revision,
        "restored lifecycle revision",
    )?)
    .bind(campaign.lifecycle_state.as_str())
    .bind(campaign.archived_at.as_deref())
    .bind(&campaign.safety_policy_id)
    .bind(&campaign.progression_policy_id)
    .bind(&campaign.retention_class)
    .bind(campaign.retention_delete_after.as_deref())
    .execute(&mut **transaction)
    .await
    .map_err(|error| map_lifecycle_insert(error, "campaign session", &document.id))?;

    for character in &exported.characters {
        sqlx::query(
            "INSERT INTO characters
             (id, campaign_session_id, schema_version, revision, payload_json,
              created_at, updated_at)
             VALUES ($1, $2, $3, $4, $5::jsonb, $6::timestamptz, $7::timestamptz)",
        )
        .bind(&character.id)
        .bind(&document.id)
        .bind(i64::from(character.schema_version))
        .bind(to_i64(character.revision, "character revision")?)
        .bind(serialize("character", &character.value)?)
        .bind(&character.created_at)
        .bind(&character.updated_at)
        .execute(&mut **transaction)
        .await
        .map_err(|error| map_lifecycle_insert(error, "character", &character.id))?;
    }
    for turn in &exported.turns {
        sqlx::query(
            "INSERT INTO turn_audits
             (id, campaign_session_id, turn_number, actor_id, schema_version,
              payload_json, created_at, correlation_id)
             VALUES ($1, $2, $3, $4, $5, $6::jsonb, $7::timestamptz, $8)",
        )
        .bind(&turn.id)
        .bind(&document.id)
        .bind(to_i64(turn.turn_number, "turn number")?)
        .bind(turn.actor_id.as_deref())
        .bind(i64::from(turn.schema_version))
        .bind(serialize("turn audit", &turn.event)?)
        .bind(&turn.created_at)
        .bind(turn.correlation_id.as_deref())
        .execute(&mut **transaction)
        .await
        .map_err(|error| map_lifecycle_insert(error, "turn audit", &turn.id))?;
    }
    for recap in &exported.private_recaps {
        sqlx::query(
            "INSERT INTO campaign_private_recaps
             (id, campaign_session_id, owner_key, schema_version,
              campaign_revision, idempotency_key, request_fingerprint,
              first_turn_number, last_turn_number, source_audit_count,
              source_audit_digest, template_id, body, body_digest, created_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12,
                     $13, $14, $15::timestamptz)",
        )
        .bind(&recap.id)
        .bind(&document.id)
        .bind(&exported.owner_key)
        .bind(i64::from(recap.schema_version))
        .bind(to_i64(
            recap.campaign_revision,
            "private recap campaign revision",
        )?)
        .bind(&recap.idempotency_key)
        .bind(recap.request_fingerprint.as_str())
        .bind(
            recap
                .first_turn_number
                .map(|value| to_i64(value, "private recap first turn"))
                .transpose()?,
        )
        .bind(
            recap
                .last_turn_number
                .map(|value| to_i64(value, "private recap last turn"))
                .transpose()?,
        )
        .bind(to_i64(
            recap.source_audit_count,
            "private recap source audit count",
        )?)
        .bind(recap.source_audit_digest.as_str())
        .bind(&recap.template_id)
        .bind(&recap.body)
        .bind(recap.body_digest.as_str())
        .bind(&recap.created_at)
        .execute(&mut **transaction)
        .await
        .map_err(|error| map_lifecycle_insert(error, "private campaign recap", &recap.id))?;
    }
    for receipt in &exported.command_receipts {
        sqlx::query(
            "INSERT INTO command_receipts
             (campaign_session_id, idempotency_key, command_kind,
              request_fingerprint, expected_revision, result_revision,
              audit_id, response_json, created_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9::timestamptz)",
        )
        .bind(&document.id)
        .bind(&receipt.idempotency_key)
        .bind(&receipt.command_kind)
        .bind(receipt.request_fingerprint.as_str())
        .bind(to_i64(receipt.expected_revision, "expected revision")?)
        .bind(to_i64(receipt.result_revision, "result revision")?)
        .bind(&receipt.audit_id)
        .bind(serialize("command receipt response", &receipt.response)?)
        .bind(&receipt.created_at)
        .execute(&mut **transaction)
        .await
        .map_err(|error| {
            map_lifecycle_insert(error, "command receipt", &receipt.idempotency_key)
        })?;
    }
    for receipt in &exported.text_presentation_receipts {
        sqlx::query(
            "INSERT INTO generated_text_presentation_receipts
             (campaign_session_id, origin_turn_id, schema_version,
              client_idempotency_key, presentation_id, generation_job_id,
              generation_attempt_id, version, source, config_digest,
              prompt_digest, policy_digest, output_digest, created_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12,
                     $13, $14::timestamptz)",
        )
        .bind(&document.id)
        .bind(&receipt.origin_turn_id)
        .bind(i64::from(receipt.schema_version))
        .bind(&receipt.client_idempotency_key)
        .bind(&receipt.presentation_id)
        .bind(&receipt.generation_job_id)
        .bind(&receipt.generation_attempt_id)
        .bind(i16::from(receipt.version))
        .bind(&receipt.source)
        .bind(receipt.config_digest.as_str())
        .bind(receipt.prompt_digest.as_str())
        .bind(receipt.policy_digest.as_str())
        .bind(receipt.output_digest.as_str())
        .bind(&receipt.created_at)
        .execute(&mut **transaction)
        .await
        .map_err(|error| {
            map_lifecycle_insert(
                error,
                "generated text presentation receipt",
                &receipt.presentation_id,
            )
        })?;
    }
    for receipt in &exported.typed_intent_receipts {
        sqlx::query(
            "INSERT INTO typed_intent_command_receipts
             (campaign_session_id, client_idempotency_key, schema_version,
              player_intent_digest, expected_campaign_revision,
              expected_encounter_revision, resolved_intent_json,
              interpretation_label, interpretation_evidence_json, state,
              origin_turn_id, event_sequence, result_campaign_revision,
              created_at, updated_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7::jsonb, $8, $9::jsonb,
                     $10, $11, $12, $13, $14::timestamptz, $15::timestamptz)",
        )
        .bind(&document.id)
        .bind(&receipt.client_idempotency_key)
        .bind(i64::from(receipt.schema_version))
        .bind(receipt.player_intent_digest.as_str())
        .bind(to_i64(
            receipt.expected_campaign_revision,
            "typed intent expected campaign revision",
        )?)
        .bind(to_i64(
            receipt.expected_encounter_revision,
            "typed intent expected encounter revision",
        )?)
        .bind(serialize(
            "typed intent receipt resolved intent",
            &receipt.resolved_intent,
        )?)
        .bind(&receipt.interpretation_label)
        .bind(serialize(
            "typed intent receipt interpretation evidence",
            &receipt.interpretation_evidence,
        )?)
        .bind(&receipt.state)
        .bind(receipt.origin_turn_id.as_deref())
        .bind(
            receipt
                .event_sequence
                .map(|value| to_i64(value, "typed intent event sequence"))
                .transpose()?,
        )
        .bind(
            receipt
                .result_campaign_revision
                .map(|value| to_i64(value, "typed intent result campaign revision"))
                .transpose()?,
        )
        .bind(&receipt.created_at)
        .bind(&receipt.updated_at)
        .execute(&mut **transaction)
        .await
        .map_err(|error| {
            map_lifecycle_insert(
                error,
                "typed intent command receipt",
                &receipt.client_idempotency_key,
            )
        })?;
    }
    if let Some(pins) = &exported.content_pins {
        let seal_reason = match pins.evidence.seal_reason {
            CampaignPinSealReason::SelectedTheme => "selected_theme",
            CampaignPinSealReason::LegacySelectedTheme => "legacy_selected_theme",
            CampaignPinSealReason::LegacyDigestAlias => "legacy_digest_alias",
            CampaignPinSealReason::LegacyDefaultRainbound => "legacy_default_rainbound",
        };
        let legacy_json = pins
            .evidence
            .legacy_source
            .as_ref()
            .map(|legacy| serialize("legacy hero pins", legacy))
            .transpose()?;
        sqlx::query(
            "INSERT INTO campaign_content_pins
             (campaign_session_id, schema_version, seal_reason, payload_json,
              legacy_source_json, created_at)
             VALUES ($1, $2, $3, $4::jsonb, $5::jsonb, $6::timestamptz)",
        )
        .bind(&document.id)
        .bind(i64::from(CAMPAIGN_PINS_SCHEMA_VERSION))
        .bind(seal_reason)
        .bind(serialize("campaign content pins", &pins.evidence.pins)?)
        .bind(legacy_json)
        .bind(&pins.created_at)
        .execute(&mut **transaction)
        .await
        .map_err(|error| map_lifecycle_insert(error, "campaign content pins", &document.id))?;
    }
    for draft in &exported.hero_drafts {
        sqlx::query(
            "INSERT INTO hero_creation_drafts
             (id, campaign_session_id, owner_key, schema_version, revision,
              expires_at_epoch_seconds, retention_delete_after_epoch_seconds,
              payload_json, created_at, updated_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8::jsonb,
                     $9::timestamptz, $10::timestamptz)",
        )
        .bind(&draft.document.id)
        .bind(&document.id)
        .bind(&exported.owner_key)
        .bind(i64::from(draft.document.schema_version))
        .bind(to_i64(draft.document.revision, "hero draft revision")?)
        .bind(to_i64(draft.expires_at_epoch_seconds, "hero draft expiry")?)
        .bind(to_i64(
            draft.retention_delete_after_epoch_seconds,
            "hero draft retention",
        )?)
        .bind(serialize("hero draft", &draft.document.value)?)
        .bind(&draft.document.created_at)
        .bind(&draft.document.updated_at)
        .execute(&mut **transaction)
        .await
        .map_err(|error| map_lifecycle_insert(error, "hero draft", &draft.document.id))?;
    }
    if let Some(hero) = &exported.hero_character {
        sqlx::query(
            "INSERT INTO hero_characters
             (id, campaign_session_id, owner_key, schema_version, revision,
              payload_json, created_at, updated_at)
             VALUES ($1, $2, $3, $4, $5, $6::jsonb,
                     $7::timestamptz, $8::timestamptz)",
        )
        .bind(&hero.id)
        .bind(&document.id)
        .bind(&exported.owner_key)
        .bind(i64::from(hero.schema_version))
        .bind(to_i64(hero.revision, "hero revision")?)
        .bind(serialize("hero character", &hero.value)?)
        .bind(&hero.created_at)
        .bind(&hero.updated_at)
        .execute(&mut **transaction)
        .await
        .map_err(|error| map_lifecycle_insert(error, "hero character", &hero.id))?;
    }
    for audit in &exported.hero_audits {
        sqlx::query(
            "INSERT INTO hero_audits
             (id, campaign_session_id, subject_kind, subject_id, audit_kind,
              schema_version, subject_revision, occurred_at_epoch_seconds,
              payload_json, created_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9::jsonb,
                     $10::timestamptz)",
        )
        .bind(&audit.id)
        .bind(&document.id)
        .bind(&audit.subject_kind)
        .bind(&audit.subject_id)
        .bind(&audit.audit_kind)
        .bind(i64::from(audit.schema_version))
        .bind(to_i64(audit.subject_revision, "hero audit revision")?)
        .bind(to_i64(
            audit.occurred_at_epoch_seconds,
            "hero audit timestamp",
        )?)
        .bind(serialize("hero audit", &audit.payload)?)
        .bind(&audit.created_at)
        .execute(&mut **transaction)
        .await
        .map_err(|error| map_lifecycle_insert(error, "hero audit", &audit.id))?;
    }
    for receipt in &exported.hero_receipts {
        sqlx::query(
            "INSERT INTO hero_command_receipts
             (scope_kind, scope_id, campaign_session_id, idempotency_key,
              command_kind, request_fingerprint, expected_revision,
              result_revision, audit_id, response_json, created_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10,
                     $11::timestamptz)",
        )
        .bind(&receipt.scope_kind)
        .bind(&receipt.scope_id)
        .bind(&document.id)
        .bind(&receipt.idempotency_key)
        .bind(&receipt.command_kind)
        .bind(receipt.request_fingerprint.as_str())
        .bind(to_i64(receipt.expected_revision, "hero expected revision")?)
        .bind(to_i64(receipt.result_revision, "hero result revision")?)
        .bind(&receipt.audit_id)
        .bind(serialize("hero receipt response", &receipt.response)?)
        .bind(&receipt.created_at)
        .execute(&mut **transaction)
        .await
        .map_err(|error| {
            map_lifecycle_insert(error, "hero command receipt", &receipt.idempotency_key)
        })?;
    }
    for claim in &exported.encounter_reward_claims {
        sqlx::query(
            "INSERT INTO encounter_reward_claims
             (campaign_session_id, encounter_id, character_id,
              encounter_revision, victory_event_sequence, reward_tier,
              experience_awarded, hero_audit_id, created_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9::timestamptz)",
        )
        .bind(&document.id)
        .bind(&claim.encounter_id)
        .bind(&claim.character_id)
        .bind(to_i64(claim.encounter_revision, "encounter revision")?)
        .bind(to_i64(
            claim.victory_event_sequence,
            "victory event sequence",
        )?)
        .bind(&claim.reward_tier)
        .bind(i64::from(claim.experience_awarded))
        .bind(&claim.hero_audit_id)
        .bind(&claim.created_at)
        .execute(&mut **transaction)
        .await
        .map_err(|error| {
            map_lifecycle_insert(error, "encounter reward claim", &claim.encounter_id)
        })?;
    }
    insert_restored_lifecycle_rows(transaction, exported).await?;
    insert_restored_presentations_and_assets(transaction, exported).await?;
    Ok(())
}

async fn insert_restored_lifecycle_rows(
    transaction: &mut Transaction<'_, Postgres>,
    exported: &CampaignPrivateExportV1,
) -> Result<(), RepositoryError> {
    let campaign_session_id = &exported.campaign.document.id;
    for play in &exported.play_sessions {
        let restored_open = play.state == "waiting" || play.state == "active";
        let state = if restored_open {
            "closed"
        } else {
            play.state.as_str()
        };
        let ended_campaign_revision = if restored_open {
            Some(exported.campaign.document.revision)
        } else {
            play.ended_campaign_revision
        };
        let closed_at = if restored_open {
            Some(exported.exported_at.as_str())
        } else {
            play.closed_at.as_deref()
        };
        let close_reason = if restored_open {
            Some("restore_import")
        } else {
            play.close_reason.as_deref()
        };
        sqlx::query(
            "INSERT INTO campaign_play_sessions
             (id, campaign_session_id, owner_key, schema_version, state,
              started_campaign_revision, ended_campaign_revision,
              opened_at, closed_at, close_reason)
             VALUES ($1, $2, $3, $4, $5, $6, $7,
                     $8::timestamptz, $9::timestamptz, $10)",
        )
        .bind(&play.id)
        .bind(campaign_session_id)
        .bind(&play.owner_key)
        .bind(i64::from(play.schema_version))
        .bind(state)
        .bind(to_i64(
            play.started_campaign_revision,
            "play session start revision",
        )?)
        .bind(
            ended_campaign_revision
                .map(|revision| to_i64(revision, "play session end revision"))
                .transpose()?,
        )
        .bind(&play.opened_at)
        .bind(closed_at)
        .bind(close_reason)
        .execute(&mut **transaction)
        .await
        .map_err(|error| map_lifecycle_insert(error, "play session", &play.id))?;
    }
    for audit in &exported.lifecycle_audits {
        sqlx::query(
            "INSERT INTO campaign_lifecycle_audits
             (id, campaign_session_id, owner_key, schema_version,
              lifecycle_revision, event_kind, payload_json, created_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7::jsonb, $8::timestamptz)",
        )
        .bind(&audit.id)
        .bind(campaign_session_id)
        .bind(&exported.owner_key)
        .bind(i64::from(CAMPAIGN_LIFECYCLE_SCHEMA_VERSION))
        .bind(to_i64(
            audit.lifecycle_revision,
            "lifecycle audit revision",
        )?)
        .bind(audit.payload.event_kind())
        .bind(serialize("campaign lifecycle audit", &audit.payload)?)
        .bind(&audit.created_at)
        .execute(&mut **transaction)
        .await
        .map_err(|error| map_lifecycle_insert(error, "campaign lifecycle audit", &audit.id))?;
    }
    Ok(())
}

async fn insert_restore_import_audit(
    transaction: &mut Transaction<'_, Postgres>,
    exported: &CampaignPrivateExportV1,
    lifecycle_revision: u64,
) -> Result<(), RepositoryError> {
    let closed_play_session_ids = exported
        .play_sessions
        .iter()
        .filter(|session| session.state == "open")
        .map(|session| session.id.clone())
        .collect::<Vec<_>>();
    let payload = LifecycleAuditPayload::RestoreImported {
        closed_play_session_ids,
    };
    let id = format!("lifecycle-{}", uuid::Uuid::new_v4());
    sqlx::query(
        "INSERT INTO campaign_lifecycle_audits
         (id, campaign_session_id, owner_key, schema_version,
          lifecycle_revision, event_kind, payload_json)
         VALUES ($1, $2, $3, $4, $5, 'restore_imported', $6::jsonb)",
    )
    .bind(&id)
    .bind(&exported.campaign.document.id)
    .bind(&exported.owner_key)
    .bind(i64::from(CAMPAIGN_LIFECYCLE_SCHEMA_VERSION))
    .bind(to_i64(lifecycle_revision, "restore lifecycle revision")?)
    .bind(serialize("restore import audit", &payload)?)
    .execute(&mut **transaction)
    .await
    .map_err(|error| map_lifecycle_insert(error, "campaign lifecycle audit", &id))?;
    Ok(())
}

async fn insert_restored_presentations_and_assets(
    transaction: &mut Transaction<'_, Postgres>,
    exported: &CampaignPrivateExportV1,
) -> Result<(), RepositoryError> {
    let campaign_session_id = &exported.campaign.document.id;
    for asset in &exported.selected_generated_assets {
        sqlx::query(
            "INSERT INTO generated_assets
             (id, campaign_session_id, turn_id, asset_kind, provider, model,
              location, prompt_fingerprint, metadata_json, created_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9::jsonb,
                     $10::timestamptz)",
        )
        .bind(&asset.id)
        .bind(campaign_session_id)
        .bind(asset.turn_id.as_deref())
        .bind(&asset.asset_kind)
        .bind(&asset.provider)
        .bind(&asset.model)
        .bind(&asset.location)
        .bind(asset.prompt_fingerprint.as_ref().map(Sha256Digest::as_str))
        .bind(serialize("generated asset metadata", &asset.metadata)?)
        .bind(&asset.created_at)
        .execute(&mut **transaction)
        .await
        .map_err(|error| map_lifecycle_insert(error, "generated asset", &asset.id))?;
    }
    for presentation in &exported.selected_text_presentations {
        sqlx::query(
            "INSERT INTO generated_text_presentations
             (id, campaign_session_id, origin_turn_id, generation_job_id,
              generation_attempt_id, client_idempotency_key, version, source, body, config_digest,
              prompt_digest, policy_digest, output_digest, selected,
              retention_delete_after, created_at, updated_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13,
                     TRUE, NULL, $14::timestamptz, $15::timestamptz)",
        )
        .bind(&presentation.id)
        .bind(campaign_session_id)
        .bind(&presentation.origin_turn_id)
        .bind(&presentation.generation_job_id)
        .bind(&presentation.generation_attempt_id)
        .bind(&presentation.client_idempotency_key)
        .bind(i16::from(presentation.version))
        .bind(&presentation.source)
        .bind(&presentation.body)
        .bind(presentation.config_digest.as_str())
        .bind(presentation.prompt_digest.as_str())
        .bind(presentation.policy_digest.as_str())
        .bind(presentation.output_digest.as_str())
        .bind(&presentation.created_at)
        .bind(&presentation.updated_at)
        .execute(&mut **transaction)
        .await
        .map_err(|error| {
            map_lifecycle_insert(error, "generated text presentation", &presentation.id)
        })?;
    }
    Ok(())
}

fn summary_from_row(row: PgRow) -> Result<CampaignSummary, RepositoryError> {
    let id: String = row.try_get("id").map_err(RepositoryError::Database)?;
    let state: String = row
        .try_get("lifecycle_state")
        .map_err(RepositoryError::Database)?;
    let summary = CampaignSummary {
        schema_version: CAMPAIGN_LIFECYCLE_SCHEMA_VERSION,
        campaign_session_id: id.clone(),
        owner_key: row
            .try_get("owner_key")
            .map_err(RepositoryError::Database)?,
        title: row.try_get("title").map_err(RepositoryError::Database)?,
        campaign_revision: from_i64(
            row.try_get("revision").map_err(RepositoryError::Database)?,
            "campaign revision",
        )?,
        lifecycle_revision: from_i64(
            row.try_get("lifecycle_revision")
                .map_err(RepositoryError::Database)?,
            "lifecycle revision",
        )?,
        lifecycle_state: CampaignLifecycleState::try_from(state.as_str())?,
        archived_at: row
            .try_get("archived_at")
            .map_err(RepositoryError::Database)?,
        safety_policy_id: row
            .try_get("safety_policy_id")
            .map_err(RepositoryError::Database)?,
        progression_policy_id: row
            .try_get("progression_policy_id")
            .map_err(RepositoryError::Database)?,
        retention_class: row
            .try_get("retention_class")
            .map_err(RepositoryError::Database)?,
        retention_delete_after: row
            .try_get("retention_delete_after")
            .map_err(RepositoryError::Database)?,
        open_play_session_id: row
            .try_get("open_play_session_id")
            .map_err(RepositoryError::Database)?,
        created_at: row
            .try_get("created_at")
            .map_err(RepositoryError::Database)?,
        updated_at: row
            .try_get("updated_at")
            .map_err(RepositoryError::Database)?,
    };
    validate_summary(&summary)?;
    Ok(summary)
}

fn validate_summary(summary: &CampaignSummary) -> Result<(), RepositoryError> {
    if !is_valid_opaque_id(&summary.campaign_session_id)
        || !is_valid_opaque_id(&summary.owner_key)
        || summary.title.trim().is_empty()
        || summary.campaign_revision == 0
        || summary.lifecycle_revision == 0
        || !is_valid_opaque_id(&summary.safety_policy_id)
        || !is_valid_opaque_id(&summary.progression_policy_id)
        || summary
            .open_play_session_id
            .as_deref()
            .is_some_and(|id| !is_valid_opaque_id(id))
        || summary.created_at.is_empty()
        || summary.updated_at.is_empty()
        || !matches!(
            (
                summary.lifecycle_state,
                summary.archived_at.is_some(),
                summary.retention_class.as_str(),
                summary.retention_delete_after.is_some(),
            ),
            (
                CampaignLifecycleState::Active,
                false,
                "campaign_lifetime",
                false
            ) | (
                CampaignLifecycleState::Archived,
                true,
                "archived_owner_managed",
                false
            )
        )
    {
        return invalid(
            "campaign summary",
            &summary.campaign_session_id,
            "stored lifecycle metadata is inconsistent",
        );
    }
    Ok(())
}

fn turn_history_from_row(row: PgRow) -> Result<CampaignTurnHistoryItem, RepositoryError> {
    let id: String = row.try_get("id").map_err(RepositoryError::Database)?;
    let payload_json: String = row
        .try_get("payload_json")
        .map_err(RepositoryError::Database)?;
    let event: SessionEventDto = serde_json::from_str(&payload_json).map_err(|source| {
        RepositoryError::InvalidStoredData {
            entity: "turn audit",
            id: id.clone(),
            source,
        }
    })?;
    event
        .validate()
        .map_err(|source| RepositoryError::CoreValidation {
            entity: "turn audit",
            id: id.clone(),
            source,
        })?;
    let turn_number = from_i64(
        row.try_get("turn_number")
            .map_err(RepositoryError::Database)?,
        "turn number",
    )?;
    let campaign_session_id: String = row
        .try_get("campaign_session_id")
        .map_err(RepositoryError::Database)?;
    if event.session_id != campaign_session_id || event.sequence != turn_number {
        return invalid(
            "turn audit",
            &id,
            "row identity and validated event envelope do not match",
        );
    }
    Ok(CampaignTurnHistoryItem {
        schema_version: from_i64_u32(
            row.try_get("schema_version")
                .map_err(RepositoryError::Database)?,
            "turn schema version",
        )?,
        id,
        campaign_session_id,
        turn_number,
        actor_id: row.try_get("actor_id").map_err(RepositoryError::Database)?,
        correlation_id: row
            .try_get("correlation_id")
            .map_err(RepositoryError::Database)?,
        event,
        created_at: row
            .try_get("created_at")
            .map_err(RepositoryError::Database)?,
    })
}

fn play_session_from_row(row: PgRow) -> Result<CampaignPlaySession, RepositoryError> {
    let play = CampaignPlaySession {
        schema_version: u16::try_from(from_i64_u32(
            row.try_get("schema_version")
                .map_err(RepositoryError::Database)?,
            "play session schema version",
        )?)
        .map_err(|_| RepositoryError::NumericRange {
            field: "play session schema version",
        })?,
        id: row.try_get("id").map_err(RepositoryError::Database)?,
        campaign_session_id: row
            .try_get("campaign_session_id")
            .map_err(RepositoryError::Database)?,
        owner_key: row
            .try_get("owner_key")
            .map_err(RepositoryError::Database)?,
        state: row.try_get("state").map_err(RepositoryError::Database)?,
        started_campaign_revision: from_i64(
            row.try_get("started_campaign_revision")
                .map_err(RepositoryError::Database)?,
            "play session start revision",
        )?,
        ended_campaign_revision: row
            .try_get::<Option<i64>, _>("ended_campaign_revision")
            .map_err(RepositoryError::Database)?
            .map(|value| from_i64(value, "play session end revision"))
            .transpose()?,
        opened_at: row
            .try_get("opened_at")
            .map_err(RepositoryError::Database)?,
        closed_at: row
            .try_get("closed_at")
            .map_err(RepositoryError::Database)?,
        close_reason: row
            .try_get("close_reason")
            .map_err(RepositoryError::Database)?,
    };
    validate_play_session(&play)?;
    Ok(play)
}

fn validate_play_session(play: &CampaignPlaySession) -> Result<(), RepositoryError> {
    let shape_valid = match play.state.as_str() {
        "open" | "waiting" => {
            play.ended_campaign_revision.is_none()
                && play.closed_at.is_none()
                && play.close_reason.is_none()
        }
        "active" => play.ended_campaign_revision.is_none() && play.closed_at.is_none(),
        "closed" => {
            play.ended_campaign_revision
                .is_some_and(|end| end >= play.started_campaign_revision)
                && play.closed_at.is_some()
                && matches!(
                    play.close_reason.as_deref(),
                    Some("owner_ended" | "archive" | "restore_import")
                )
        }
        _ => false,
    };
    if play.schema_version != CAMPAIGN_LIFECYCLE_SCHEMA_VERSION
        || !is_valid_opaque_id(&play.id)
        || !is_valid_opaque_id(&play.campaign_session_id)
        || !is_valid_opaque_id(&play.owner_key)
        || play.started_campaign_revision == 0
        || play.opened_at.is_empty()
        || !shape_valid
    {
        return invalid(
            "campaign play session",
            &play.id,
            "stored play session is invalid",
        );
    }
    Ok(())
}

fn exported_campaign_from_row(row: PgRow) -> Result<ExportedCampaignDocument, RepositoryError> {
    let owner_key: String = row
        .try_get("owner_key")
        .map_err(RepositoryError::Database)?;
    let state: String = row
        .try_get("lifecycle_state")
        .map_err(RepositoryError::Database)?;
    Ok(ExportedCampaignDocument {
        document: exported_document_from_row(&row, "campaign session")?,
        owner_key,
        lifecycle_revision: from_i64(
            row.try_get("lifecycle_revision")
                .map_err(RepositoryError::Database)?,
            "lifecycle revision",
        )?,
        lifecycle_state: CampaignLifecycleState::try_from(state.as_str())?,
        archived_at: row
            .try_get("archived_at")
            .map_err(RepositoryError::Database)?,
        safety_policy_id: row
            .try_get("safety_policy_id")
            .map_err(RepositoryError::Database)?,
        progression_policy_id: row
            .try_get("progression_policy_id")
            .map_err(RepositoryError::Database)?,
        retention_class: row
            .try_get("retention_class")
            .map_err(RepositoryError::Database)?,
        retention_delete_after: row
            .try_get("retention_delete_after")
            .map_err(RepositoryError::Database)?,
    })
}

fn exported_document_from_row<T>(
    row: &PgRow,
    entity: &'static str,
) -> Result<ExportedDocument<T>, RepositoryError>
where
    T: for<'de> Deserialize<'de>,
{
    let id: String = row.try_get("id").map_err(RepositoryError::Database)?;
    let payload_json: String = row
        .try_get("payload_json")
        .map_err(RepositoryError::Database)?;
    let value = serde_json::from_str(&payload_json).map_err(|source| {
        RepositoryError::InvalidStoredData {
            entity,
            id: id.clone(),
            source,
        }
    })?;
    Ok(ExportedDocument {
        id,
        schema_version: from_i64_u32(
            row.try_get("schema_version")
                .map_err(RepositoryError::Database)?,
            "document schema version",
        )?,
        revision: from_i64(
            row.try_get("revision").map_err(RepositoryError::Database)?,
            "document revision",
        )?,
        value,
        created_at: row
            .try_get("created_at")
            .map_err(RepositoryError::Database)?,
        updated_at: row
            .try_get("updated_at")
            .map_err(RepositoryError::Database)?,
    })
}

fn exported_hero_draft_from_row(row: PgRow) -> Result<ExportedHeroDraft, RepositoryError> {
    Ok(ExportedHeroDraft {
        document: exported_document_from_row(&row, "hero draft")?,
        expires_at_epoch_seconds: from_i64(
            row.try_get("expires_at_epoch_seconds")
                .map_err(RepositoryError::Database)?,
            "hero draft expiry",
        )?,
        retention_delete_after_epoch_seconds: from_i64(
            row.try_get("retention_delete_after_epoch_seconds")
                .map_err(RepositoryError::Database)?,
            "hero draft retention",
        )?,
    })
}

fn exported_turn_from_row(row: PgRow) -> Result<ExportedTurnAudit, RepositoryError> {
    let id: String = row.try_get("id").map_err(RepositoryError::Database)?;
    let payload_json: String = row
        .try_get("payload_json")
        .map_err(RepositoryError::Database)?;
    let event = serde_json::from_str(&payload_json).map_err(|source| {
        RepositoryError::InvalidStoredData {
            entity: "turn audit",
            id: id.clone(),
            source,
        }
    })?;
    Ok(ExportedTurnAudit {
        id,
        turn_number: from_i64(
            row.try_get("turn_number")
                .map_err(RepositoryError::Database)?,
            "turn number",
        )?,
        actor_id: row.try_get("actor_id").map_err(RepositoryError::Database)?,
        correlation_id: row
            .try_get("correlation_id")
            .map_err(RepositoryError::Database)?,
        schema_version: from_i64_u32(
            row.try_get("schema_version")
                .map_err(RepositoryError::Database)?,
            "turn schema version",
        )?,
        event,
        created_at: row
            .try_get("created_at")
            .map_err(RepositoryError::Database)?,
    })
}

fn exported_command_receipt_from_row(
    row: PgRow,
) -> Result<ExportedCommandReceipt, RepositoryError> {
    let idempotency_key: String = row
        .try_get("idempotency_key")
        .map_err(RepositoryError::Database)?;
    let response_json: String = row
        .try_get("response_json")
        .map_err(RepositoryError::Database)?;
    Ok(ExportedCommandReceipt {
        idempotency_key: idempotency_key.clone(),
        command_kind: row
            .try_get("command_kind")
            .map_err(RepositoryError::Database)?,
        request_fingerprint: parse_digest_row(
            &row,
            "request_fingerprint",
            "command receipt",
            &idempotency_key,
        )?,
        expected_revision: from_i64(
            row.try_get("expected_revision")
                .map_err(RepositoryError::Database)?,
            "expected revision",
        )?,
        result_revision: from_i64(
            row.try_get("result_revision")
                .map_err(RepositoryError::Database)?,
            "result revision",
        )?,
        audit_id: row.try_get("audit_id").map_err(RepositoryError::Database)?,
        response: serde_json::from_str(&response_json).map_err(|source| {
            RepositoryError::InvalidStoredData {
                entity: "command receipt response",
                id: idempotency_key,
                source,
            }
        })?,
        created_at: row
            .try_get("created_at")
            .map_err(RepositoryError::Database)?,
    })
}

fn exported_text_presentation_receipt_from_row(
    row: PgRow,
) -> Result<ExportedTextPresentationReceipt, RepositoryError> {
    let presentation_id: String = row
        .try_get("presentation_id")
        .map_err(RepositoryError::Database)?;
    Ok(ExportedTextPresentationReceipt {
        schema_version: u16::try_from(from_i64_u32(
            row.try_get("schema_version")
                .map_err(RepositoryError::Database)?,
            "text presentation receipt schema version",
        )?)
        .map_err(|_| RepositoryError::NumericRange {
            field: "text presentation receipt schema version",
        })?,
        origin_turn_id: row
            .try_get("origin_turn_id")
            .map_err(RepositoryError::Database)?,
        client_idempotency_key: row
            .try_get("client_idempotency_key")
            .map_err(RepositoryError::Database)?,
        presentation_id: presentation_id.clone(),
        generation_job_id: row
            .try_get("generation_job_id")
            .map_err(RepositoryError::Database)?,
        generation_attempt_id: row
            .try_get("generation_attempt_id")
            .map_err(RepositoryError::Database)?,
        version: u8::try_from(
            row.try_get::<i16, _>("version")
                .map_err(RepositoryError::Database)?,
        )
        .map_err(|_| RepositoryError::NumericRange {
            field: "text presentation receipt version",
        })?,
        source: row.try_get("source").map_err(RepositoryError::Database)?,
        config_digest: parse_digest_row(
            &row,
            "config_digest",
            "text presentation receipt",
            &presentation_id,
        )?,
        prompt_digest: parse_digest_row(
            &row,
            "prompt_digest",
            "text presentation receipt",
            &presentation_id,
        )?,
        policy_digest: parse_digest_row(
            &row,
            "policy_digest",
            "text presentation receipt",
            &presentation_id,
        )?,
        output_digest: parse_digest_row(
            &row,
            "output_digest",
            "text presentation receipt",
            &presentation_id,
        )?,
        created_at: row
            .try_get("created_at")
            .map_err(RepositoryError::Database)?,
    })
}

fn exported_typed_intent_receipt_from_row(
    row: PgRow,
) -> Result<ExportedTypedIntentReceipt, RepositoryError> {
    let idempotency_key: String = row
        .try_get("client_idempotency_key")
        .map_err(RepositoryError::Database)?;
    let resolved_intent_json: String = row
        .try_get("resolved_intent_json")
        .map_err(RepositoryError::Database)?;
    let interpretation_evidence_json: String = row
        .try_get("interpretation_evidence_json")
        .map_err(RepositoryError::Database)?;
    Ok(ExportedTypedIntentReceipt {
        schema_version: u16::try_from(from_i64_u32(
            row.try_get("schema_version")
                .map_err(RepositoryError::Database)?,
            "typed intent receipt schema version",
        )?)
        .map_err(|_| RepositoryError::NumericRange {
            field: "typed intent receipt schema version",
        })?,
        client_idempotency_key: idempotency_key.clone(),
        player_intent_digest: parse_digest_row(
            &row,
            "player_intent_digest",
            "typed intent receipt",
            &idempotency_key,
        )?,
        expected_campaign_revision: from_i64(
            row.try_get("expected_campaign_revision")
                .map_err(RepositoryError::Database)?,
            "typed intent expected campaign revision",
        )?,
        expected_encounter_revision: from_i64(
            row.try_get("expected_encounter_revision")
                .map_err(RepositoryError::Database)?,
            "typed intent expected encounter revision",
        )?,
        resolved_intent: serde_json::from_str(&resolved_intent_json).map_err(|source| {
            RepositoryError::InvalidStoredData {
                entity: "typed intent receipt resolved intent",
                id: idempotency_key.clone(),
                source,
            }
        })?,
        interpretation_label: row
            .try_get("interpretation_label")
            .map_err(RepositoryError::Database)?,
        interpretation_evidence: serde_json::from_str(&interpretation_evidence_json).map_err(
            |source| RepositoryError::InvalidStoredData {
                entity: "typed intent receipt interpretation evidence",
                id: idempotency_key.clone(),
                source,
            },
        )?,
        state: row.try_get("state").map_err(RepositoryError::Database)?,
        origin_turn_id: row
            .try_get("origin_turn_id")
            .map_err(RepositoryError::Database)?,
        event_sequence: row
            .try_get::<Option<i64>, _>("event_sequence")
            .map_err(RepositoryError::Database)?
            .map(|value| from_i64(value, "typed intent event sequence"))
            .transpose()?,
        result_campaign_revision: row
            .try_get::<Option<i64>, _>("result_campaign_revision")
            .map_err(RepositoryError::Database)?
            .map(|value| from_i64(value, "typed intent result campaign revision"))
            .transpose()?,
        created_at: row
            .try_get("created_at")
            .map_err(RepositoryError::Database)?,
        updated_at: row
            .try_get("updated_at")
            .map_err(RepositoryError::Database)?,
    })
}

fn exported_hero_audit_from_row(row: PgRow) -> Result<ExportedHeroAudit, RepositoryError> {
    let id: String = row.try_get("id").map_err(RepositoryError::Database)?;
    let payload_json: String = row
        .try_get("payload_json")
        .map_err(RepositoryError::Database)?;
    Ok(ExportedHeroAudit {
        id: id.clone(),
        subject_kind: row
            .try_get("subject_kind")
            .map_err(RepositoryError::Database)?,
        subject_id: row
            .try_get("subject_id")
            .map_err(RepositoryError::Database)?,
        audit_kind: row
            .try_get("audit_kind")
            .map_err(RepositoryError::Database)?,
        schema_version: from_i64_u32(
            row.try_get("schema_version")
                .map_err(RepositoryError::Database)?,
            "hero audit schema version",
        )?,
        subject_revision: from_i64(
            row.try_get("subject_revision")
                .map_err(RepositoryError::Database)?,
            "hero audit revision",
        )?,
        occurred_at_epoch_seconds: from_i64(
            row.try_get("occurred_at_epoch_seconds")
                .map_err(RepositoryError::Database)?,
            "hero audit timestamp",
        )?,
        payload: serde_json::from_str(&payload_json).map_err(|source| {
            RepositoryError::InvalidStoredData {
                entity: "hero audit",
                id,
                source,
            }
        })?,
        created_at: row
            .try_get("created_at")
            .map_err(RepositoryError::Database)?,
    })
}

fn exported_hero_receipt_from_row(row: PgRow) -> Result<ExportedHeroReceipt, RepositoryError> {
    let idempotency_key: String = row
        .try_get("idempotency_key")
        .map_err(RepositoryError::Database)?;
    let response_json: String = row
        .try_get("response_json")
        .map_err(RepositoryError::Database)?;
    Ok(ExportedHeroReceipt {
        scope_kind: row
            .try_get("scope_kind")
            .map_err(RepositoryError::Database)?,
        scope_id: row.try_get("scope_id").map_err(RepositoryError::Database)?,
        idempotency_key: idempotency_key.clone(),
        command_kind: row
            .try_get("command_kind")
            .map_err(RepositoryError::Database)?,
        request_fingerprint: parse_digest_row(
            &row,
            "request_fingerprint",
            "hero receipt",
            &idempotency_key,
        )?,
        expected_revision: from_i64(
            row.try_get("expected_revision")
                .map_err(RepositoryError::Database)?,
            "hero expected revision",
        )?,
        result_revision: from_i64(
            row.try_get("result_revision")
                .map_err(RepositoryError::Database)?,
            "hero result revision",
        )?,
        audit_id: row.try_get("audit_id").map_err(RepositoryError::Database)?,
        response: serde_json::from_str(&response_json).map_err(|source| {
            RepositoryError::InvalidStoredData {
                entity: "hero receipt response",
                id: idempotency_key,
                source,
            }
        })?,
        created_at: row
            .try_get("created_at")
            .map_err(RepositoryError::Database)?,
    })
}

fn exported_reward_claim_from_row(
    row: PgRow,
) -> Result<ExportedEncounterRewardClaim, RepositoryError> {
    Ok(ExportedEncounterRewardClaim {
        encounter_id: row
            .try_get("encounter_id")
            .map_err(RepositoryError::Database)?,
        character_id: row
            .try_get("character_id")
            .map_err(RepositoryError::Database)?,
        encounter_revision: from_i64(
            row.try_get("encounter_revision")
                .map_err(RepositoryError::Database)?,
            "encounter revision",
        )?,
        victory_event_sequence: from_i64(
            row.try_get("victory_event_sequence")
                .map_err(RepositoryError::Database)?,
            "victory event sequence",
        )?,
        reward_tier: row
            .try_get("reward_tier")
            .map_err(RepositoryError::Database)?,
        experience_awarded: u32::try_from(from_i64(
            row.try_get("experience_awarded")
                .map_err(RepositoryError::Database)?,
            "experience awarded",
        )?)
        .map_err(|_| RepositoryError::NumericRange {
            field: "experience awarded",
        })?,
        hero_audit_id: row
            .try_get("hero_audit_id")
            .map_err(RepositoryError::Database)?,
        created_at: row
            .try_get("created_at")
            .map_err(RepositoryError::Database)?,
    })
}

fn exported_pins_from_row(row: PgRow) -> Result<ExportedCampaignPins, RepositoryError> {
    let seal_reason: String = row
        .try_get("seal_reason")
        .map_err(RepositoryError::Database)?;
    let seal_reason = match seal_reason.as_str() {
        "selected_theme" => CampaignPinSealReason::SelectedTheme,
        "legacy_selected_theme" => CampaignPinSealReason::LegacySelectedTheme,
        "legacy_digest_alias" => CampaignPinSealReason::LegacyDigestAlias,
        "legacy_default_rainbound" => CampaignPinSealReason::LegacyDefaultRainbound,
        _ => {
            return invalid(
                "campaign content pins",
                "seal-reason",
                "stored seal reason is unsupported",
            );
        }
    };
    let payload_json: String = row
        .try_get("payload_json")
        .map_err(RepositoryError::Database)?;
    let legacy_json: Option<String> = row
        .try_get("legacy_source_json")
        .map_err(RepositoryError::Database)?;
    let evidence = SealedCampaignPins {
        seal_reason,
        pins: serde_json::from_str::<CampaignContentPins>(&payload_json).map_err(|source| {
            RepositoryError::InvalidStoredData {
                entity: "campaign content pins",
                id: "payload".to_owned(),
                source,
            }
        })?,
        legacy_source: legacy_json
            .map(|value| {
                serde_json::from_str::<HeroPins>(&value).map_err(|source| {
                    RepositoryError::InvalidStoredData {
                        entity: "legacy hero pins",
                        id: "payload".to_owned(),
                        source,
                    }
                })
            })
            .transpose()?,
    };
    evidence
        .validate()
        .map_err(|source| RepositoryError::CoreValidation {
            entity: "campaign content pins",
            id: "payload".to_owned(),
            source,
        })?;
    Ok(ExportedCampaignPins {
        evidence,
        created_at: row
            .try_get("created_at")
            .map_err(RepositoryError::Database)?,
    })
}

fn exported_lifecycle_audit_from_row(
    row: PgRow,
) -> Result<ExportedLifecycleAudit, RepositoryError> {
    let id: String = row.try_get("id").map_err(RepositoryError::Database)?;
    let event_kind: String = row
        .try_get("event_kind")
        .map_err(RepositoryError::Database)?;
    let payload_json: String = row
        .try_get("payload_json")
        .map_err(RepositoryError::Database)?;
    let payload: LifecycleAuditPayload = serde_json::from_str(&payload_json).map_err(|source| {
        RepositoryError::InvalidStoredData {
            entity: "campaign lifecycle audit",
            id: id.clone(),
            source,
        }
    })?;
    if payload.event_kind() != event_kind || !payload.validate() {
        return invalid(
            "campaign lifecycle audit",
            &id,
            "event kind and payload do not match",
        );
    }
    Ok(ExportedLifecycleAudit {
        id,
        lifecycle_revision: from_i64(
            row.try_get("lifecycle_revision")
                .map_err(RepositoryError::Database)?,
            "lifecycle audit revision",
        )?,
        payload,
        created_at: row
            .try_get("created_at")
            .map_err(RepositoryError::Database)?,
    })
}

fn exported_presentation_from_row(row: PgRow) -> Result<ExportedTextPresentation, RepositoryError> {
    let id: String = row.try_get("id").map_err(RepositoryError::Database)?;
    Ok(ExportedTextPresentation {
        id: id.clone(),
        origin_turn_id: row
            .try_get("origin_turn_id")
            .map_err(RepositoryError::Database)?,
        generation_job_id: row
            .try_get("generation_job_id")
            .map_err(RepositoryError::Database)?,
        generation_attempt_id: row
            .try_get("generation_attempt_id")
            .map_err(RepositoryError::Database)?,
        client_idempotency_key: row
            .try_get("client_idempotency_key")
            .map_err(RepositoryError::Database)?,
        version: u8::try_from(
            row.try_get::<i16, _>("version")
                .map_err(RepositoryError::Database)?,
        )
        .map_err(|_| RepositoryError::NumericRange {
            field: "text presentation version",
        })?,
        source: row.try_get("source").map_err(RepositoryError::Database)?,
        body: row.try_get("body").map_err(RepositoryError::Database)?,
        config_digest: parse_digest_row(&row, "config_digest", "text presentation", &id)?,
        prompt_digest: parse_digest_row(&row, "prompt_digest", "text presentation", &id)?,
        policy_digest: parse_digest_row(&row, "policy_digest", "text presentation", &id)?,
        output_digest: parse_digest_row(&row, "output_digest", "text presentation", &id)?,
        created_at: row
            .try_get("created_at")
            .map_err(RepositoryError::Database)?,
        updated_at: row
            .try_get("updated_at")
            .map_err(RepositoryError::Database)?,
    })
}

fn exported_asset_from_row(row: PgRow) -> Result<ExportedGeneratedAsset, RepositoryError> {
    let id: String = row.try_get("id").map_err(RepositoryError::Database)?;
    let prompt: Option<String> = row
        .try_get("prompt_fingerprint")
        .map_err(RepositoryError::Database)?;
    let metadata_json: String = row
        .try_get("metadata_json")
        .map_err(RepositoryError::Database)?;
    Ok(ExportedGeneratedAsset {
        id: id.clone(),
        turn_id: row.try_get("turn_id").map_err(RepositoryError::Database)?,
        asset_kind: row
            .try_get("asset_kind")
            .map_err(RepositoryError::Database)?,
        provider: row.try_get("provider").map_err(RepositoryError::Database)?,
        model: row.try_get("model").map_err(RepositoryError::Database)?,
        location: row.try_get("location").map_err(RepositoryError::Database)?,
        prompt_fingerprint: prompt
            .map(|value| parse_digest(&value, "generated asset", &id))
            .transpose()?,
        metadata: serde_json::from_str(&metadata_json).map_err(|source| {
            RepositoryError::InvalidStoredData {
                entity: "generated asset metadata",
                id: id.clone(),
                source,
            }
        })?,
        created_at: row
            .try_get("created_at")
            .map_err(RepositoryError::Database)?,
    })
}

fn parse_digest_row(
    row: &PgRow,
    column: &str,
    entity: &'static str,
    id: &str,
) -> Result<Sha256Digest, RepositoryError> {
    let value: String = row.try_get(column).map_err(RepositoryError::Database)?;
    parse_digest(&value, entity, id)
}

fn parse_digest(
    value: &str,
    entity: &'static str,
    id: &str,
) -> Result<Sha256Digest, RepositoryError> {
    Sha256Digest::new(value.to_owned()).map_err(|_| RepositoryError::InvalidDomainState {
        entity,
        id: id.to_owned(),
        reason: "stored digest is invalid",
    })
}

fn validate_export(exported: &CampaignPrivateExportV1) -> Result<(), RepositoryError> {
    if exported.schema_version != CAMPAIGN_EXPORT_SCHEMA_VERSION {
        return Err(RepositoryError::UnsupportedSchemaVersion {
            entity: "campaign private export",
            found: u32::from(exported.schema_version),
            supported: u32::from(CAMPAIGN_EXPORT_SCHEMA_VERSION),
        });
    }
    validate_owner_key(&exported.owner_key)?;
    if exported.exported_at.is_empty() {
        return invalid(
            "campaign private export",
            "exported-at",
            "export timestamp is absent",
        );
    }
    let campaign = &exported.campaign;
    let document = &campaign.document;
    if document.schema_version != u32::from(SESSION_SCHEMA_VERSION)
        || document.id != document.value.id
        || document.revision == 0
        || document.created_at.is_empty()
        || document.updated_at.is_empty()
        || campaign.owner_key != exported.owner_key
        || campaign.lifecycle_revision == 0
        || !is_valid_opaque_id(&campaign.safety_policy_id)
        || !is_valid_opaque_id(&campaign.progression_policy_id)
        || !matches!(
            (
                campaign.lifecycle_state,
                campaign.archived_at.is_some(),
                campaign.retention_class.as_str(),
                campaign.retention_delete_after.is_some(),
            ),
            (
                CampaignLifecycleState::Active,
                false,
                "campaign_lifetime",
                false
            ) | (
                CampaignLifecycleState::Archived,
                true,
                "archived_owner_managed",
                false
            )
        )
    {
        return invalid(
            "campaign private export",
            &document.id,
            "campaign document or lifecycle metadata is inconsistent",
        );
    }
    document
        .value
        .validate()
        .map_err(|source| RepositoryError::CoreValidation {
            entity: "campaign private export",
            id: document.id.clone(),
            source,
        })?;
    if document.revision != document.value.last_event_sequence.saturating_add(1) {
        return invalid(
            "campaign private export",
            &document.id,
            "campaign revision does not match its immutable event sequence",
        );
    }

    let mut character_ids = BTreeSet::new();
    for character in &exported.characters {
        if character.schema_version != CHARACTER_SCHEMA_VERSION
            || character.id != character.value.id()
            || character.revision == 0
            || !character_ids.insert(character.id.as_str())
        {
            return invalid(
                "campaign private export",
                &character.id,
                "character identity, schema, or revision is invalid",
            );
        }
        character
            .value
            .validate()
            .map_err(|source| RepositoryError::CoreValidation {
                entity: "campaign private export character",
                id: character.id.clone(),
                source,
            })?;
    }
    let declared = document
        .value
        .character_ids
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    if declared != character_ids {
        return invalid(
            "campaign private export",
            &document.id,
            "campaign roster does not match exported character documents",
        );
    }

    let mut turn_ids = BTreeSet::new();
    let mut previous_turn: u64 = 0;
    for turn in &exported.turns {
        if turn.schema_version != u32::from(SESSION_SCHEMA_VERSION)
            || !is_valid_opaque_id(&turn.id)
            || !turn_ids.insert(turn.id.as_str())
            || turn.turn_number != previous_turn.saturating_add(1)
            || turn.event.session_id != document.id
            || turn.event.sequence != turn.turn_number
        {
            return invalid(
                "campaign private export turn",
                &turn.id,
                "turn identity, ordering, schema, or envelope is invalid",
            );
        }
        turn.event
            .validate()
            .map_err(|source| RepositoryError::CoreValidation {
                entity: "campaign private export turn",
                id: turn.id.clone(),
                source,
            })?;
        previous_turn = turn.turn_number;
    }
    if previous_turn != document.value.last_event_sequence {
        return invalid(
            "campaign private export",
            &document.id,
            "turn history is incomplete",
        );
    }

    for receipt in &exported.command_receipts {
        if !is_valid_opaque_id(&receipt.idempotency_key)
            || !is_valid_opaque_id(&receipt.command_kind)
            || !turn_ids.contains(receipt.audit_id.as_str())
            || receipt.result_revision != receipt.expected_revision.saturating_add(1)
        {
            return invalid(
                "campaign private export receipt",
                &receipt.idempotency_key,
                "command receipt is inconsistent",
            );
        }
    }

    let mut text_receipt_ids = BTreeSet::new();
    let mut text_receipt_jobs = BTreeSet::new();
    let mut text_receipt_attempts = BTreeSet::new();
    let mut text_receipt_versions = BTreeSet::new();
    let mut text_receipt_keys = BTreeSet::new();
    for receipt in &exported.text_presentation_receipts {
        if receipt.schema_version != 1
            || !turn_ids.contains(receipt.origin_turn_id.as_str())
            || !is_valid_opaque_id(&receipt.client_idempotency_key)
            || !is_valid_opaque_id(&receipt.presentation_id)
            || !is_valid_opaque_id(&receipt.generation_job_id)
            || !is_valid_opaque_id(&receipt.generation_attempt_id)
            || !(1..=3).contains(&receipt.version)
            || !matches!(
                receipt.source.as_str(),
                "provider" | "authored_fallback" | "engine_authored"
            )
            || receipt.created_at.is_empty()
            || !text_receipt_ids.insert(receipt.presentation_id.as_str())
            || !text_receipt_jobs.insert(receipt.generation_job_id.as_str())
            || !text_receipt_attempts.insert(receipt.generation_attempt_id.as_str())
            || !text_receipt_versions.insert((receipt.origin_turn_id.as_str(), receipt.version))
            || !text_receipt_keys.insert((
                receipt.origin_turn_id.as_str(),
                receipt.client_idempotency_key.as_str(),
            ))
        {
            return invalid(
                "campaign private export text presentation receipt",
                &receipt.presentation_id,
                "body-free presentation receipt is inconsistent",
            );
        }
    }

    let mut typed_receipt_keys = BTreeSet::new();
    for receipt in &exported.typed_intent_receipts {
        let evidence_size = serde_json::to_vec(&receipt.interpretation_evidence)
            .map_err(|source| RepositoryError::Serialize {
                entity: "typed intent interpretation evidence",
                source,
            })?
            .len();
        let intent_is_valid = EncounterCommand::new(
            receipt.expected_encounter_revision,
            receipt.client_idempotency_key.clone(),
            receipt.resolved_intent.clone(),
        )
        .validate()
        .is_ok();
        let state_is_valid = match (
            receipt.state.as_str(),
            receipt.origin_turn_id.as_deref(),
            receipt.event_sequence,
            receipt.result_campaign_revision,
        ) {
            ("pending", None, None, None) => true,
            ("committed", Some(turn_id), Some(sequence), Some(result_revision)) => {
                turn_ids.contains(turn_id)
                    && exported
                        .turns
                        .iter()
                        .any(|turn| turn.id == turn_id && turn.turn_number == sequence)
                    && result_revision == receipt.expected_campaign_revision.saturating_add(1)
                    && result_revision <= document.revision
            }
            _ => false,
        };
        if receipt.schema_version != 1
            || !is_valid_opaque_id(&receipt.client_idempotency_key)
            || !typed_receipt_keys.insert(receipt.client_idempotency_key.as_str())
            || receipt.expected_campaign_revision == 0
            || receipt.expected_campaign_revision > document.revision
            || receipt.expected_encounter_revision == 0
            || !intent_is_valid
            || receipt.interpretation_label.trim() != receipt.interpretation_label
            || receipt.interpretation_label.is_empty()
            || receipt.interpretation_label.chars().count() > 512
            || receipt.interpretation_label.len() > 2_048
            || receipt.interpretation_label.chars().any(char::is_control)
            || !receipt.interpretation_evidence.is_object()
            || evidence_size > 32_768
            || !state_is_valid
            || receipt.created_at.is_empty()
            || receipt.updated_at.is_empty()
        {
            return invalid(
                "campaign private export typed intent receipt",
                &receipt.client_idempotency_key,
                "typed intent recovery receipt is inconsistent",
            );
        }
    }

    let mut draft_ids = BTreeSet::new();
    for draft in &exported.hero_drafts {
        if draft.document.schema_version != u32::from(HERO_DRAFT_SCHEMA_VERSION)
            || draft.document.id != draft.document.value.draft_id
            || draft.document.value.campaign_id != document.id
            || draft.document.value.owner_id != exported.owner_key
            || draft.document.revision != draft.document.value.revision.saturating_add(1)
            || draft.expires_at_epoch_seconds != draft.document.value.expires_at_epoch_seconds
            || draft.retention_delete_after_epoch_seconds < draft.expires_at_epoch_seconds
            || !draft_ids.insert(draft.document.id.as_str())
        {
            return invalid(
                "campaign private export hero draft",
                &draft.document.id,
                "hero draft metadata is inconsistent",
            );
        }
        draft
            .document
            .value
            .validate()
            .map_err(|source| RepositoryError::HeroValidation {
                entity: "campaign private export hero draft",
                id: draft.document.id.clone(),
                source,
            })?;
    }
    if let Some(hero) = &exported.hero_character {
        if hero.schema_version != u32::from(HERO_CHARACTER_SCHEMA_VERSION)
            || hero.id != hero.value.character_id
            || hero.value.campaign_id != document.id
            || hero.value.owner_id != exported.owner_key
            || hero.revision != hero.value.revision.saturating_add(1)
        {
            return invalid(
                "campaign private export hero",
                &hero.id,
                "hero metadata is inconsistent",
            );
        }
        hero.value
            .validate()
            .map_err(|source| RepositoryError::HeroValidation {
                entity: "campaign private export hero",
                id: hero.id.clone(),
                source,
            })?;
    }
    match &exported.content_pins {
        Some(pins) => {
            pins.evidence
                .validate()
                .map_err(|source| RepositoryError::CoreValidation {
                    entity: "campaign private export pins",
                    id: document.id.clone(),
                    source,
                })?
        }
        None if exported.turns.is_empty() && exported.hero_character.is_none() => {}
        None => {
            return invalid(
                "campaign private export pins",
                &document.id,
                "a playable restored campaign requires sealed validated pins",
            );
        }
    }

    let mut hero_audit_ids = BTreeSet::new();
    for audit in &exported.hero_audits {
        let (expected_subject_kind, expected_audit_kind) = match &audit.payload {
            HeroAuditPayload::CreationTransition { .. } => ("draft", "creation_transition"),
            HeroAuditPayload::RewardAwarded { .. } => ("character", "reward_awarded"),
            HeroAuditPayload::LevelUp { .. } => ("character", "level_up"),
        };
        audit.payload.validate()?;
        if audit.id != audit.payload.audit_id()
            || audit.subject_id != audit.payload.subject_id()
            || audit.subject_kind != expected_subject_kind
            || audit.audit_kind != expected_audit_kind
            || audit.subject_revision == 0
            || !hero_audit_ids.insert(audit.id.as_str())
        {
            return invalid(
                "campaign private export hero audit",
                &audit.id,
                "hero audit envelope is inconsistent",
            );
        }
    }
    for receipt in &exported.hero_receipts {
        if !matches!(receipt.scope_kind.as_str(), "draft" | "character")
            || !is_valid_opaque_id(&receipt.scope_id)
            || !is_valid_opaque_id(&receipt.idempotency_key)
            || !hero_audit_ids.contains(receipt.audit_id.as_str())
            || receipt.result_revision != receipt.expected_revision.saturating_add(1)
        {
            return invalid(
                "campaign private export hero receipt",
                &receipt.idempotency_key,
                "hero receipt is inconsistent",
            );
        }
    }
    for claim in &exported.encounter_reward_claims {
        if !is_valid_opaque_id(&claim.encounter_id)
            || !is_valid_opaque_id(&claim.character_id)
            || claim.encounter_revision == 0
            || claim.victory_event_sequence == 0
            || !matches!(claim.reward_tier.as_str(), "minor" | "major")
            || claim.experience_awarded == 0
            || !hero_audit_ids.contains(claim.hero_audit_id.as_str())
        {
            return invalid(
                "campaign private export reward claim",
                &claim.encounter_id,
                "encounter reward claim is inconsistent",
            );
        }
    }

    let mut open_count = 0;
    let mut play_ids = BTreeSet::new();
    for play in &exported.play_sessions {
        validate_play_session(play)?;
        if play.campaign_session_id != document.id
            || play.owner_key != exported.owner_key
            || !play_ids.insert(play.id.as_str())
        {
            return invalid(
                "campaign private export play session",
                &play.id,
                "play session owner or campaign is inconsistent",
            );
        }
        open_count += usize::from(play.state == "waiting");
    }
    if open_count > 1 {
        return invalid(
            "campaign private export",
            &document.id,
            "more than one play session is open",
        );
    }
    let mut recap_ids = BTreeSet::new();
    let mut recap_revisions = BTreeSet::new();
    for recap in &exported.private_recaps {
        recap.validate_for_campaign(&document.id, document.revision)?;
        if !recap_ids.insert(recap.id.as_str())
            || !recap_revisions.insert(recap.campaign_revision)
            || recap
                .last_turn_number
                .is_some_and(|turn| turn > document.value.last_event_sequence)
        {
            return invalid(
                "campaign private export recap",
                &recap.id,
                "private recap identity, revision, or audit range is inconsistent",
            );
        }
    }
    let mut lifecycle_revisions = BTreeSet::new();
    for audit in &exported.lifecycle_audits {
        if !is_valid_opaque_id(&audit.id)
            || !audit.payload.validate()
            || audit.lifecycle_revision <= 1
            || audit.lifecycle_revision > campaign.lifecycle_revision
            || !lifecycle_revisions.insert(audit.lifecycle_revision)
        {
            return invalid(
                "campaign private export lifecycle audit",
                &audit.id,
                "lifecycle audit is inconsistent",
            );
        }
    }

    let mut presentation_turns = BTreeSet::new();
    for presentation in &exported.selected_text_presentations {
        if !is_valid_opaque_id(&presentation.id)
            || !turn_ids.contains(presentation.origin_turn_id.as_str())
            || !is_valid_opaque_id(&presentation.generation_job_id)
            || !is_valid_opaque_id(&presentation.generation_attempt_id)
            || !is_valid_opaque_id(&presentation.client_idempotency_key)
            || !(1..=3).contains(&presentation.version)
            || !matches!(
                presentation.source.as_str(),
                "provider" | "authored_fallback" | "engine_authored"
            )
            || presentation.body.trim() != presentation.body
            || presentation.body.is_empty()
            || presentation.body.chars().count() > 12_000
            || !presentation_turns.insert(presentation.origin_turn_id.as_str())
        {
            return invalid(
                "campaign private export presentation",
                &presentation.id,
                "selected presentation is invalid",
            );
        }
        if !exported.text_presentation_receipts.iter().any(|receipt| {
            receipt.presentation_id == presentation.id
                && receipt.origin_turn_id == presentation.origin_turn_id
                && receipt.client_idempotency_key == presentation.client_idempotency_key
                && receipt.generation_job_id == presentation.generation_job_id
                && receipt.generation_attempt_id == presentation.generation_attempt_id
                && receipt.version == presentation.version
                && receipt.source == presentation.source
                && receipt.config_digest == presentation.config_digest
                && receipt.prompt_digest == presentation.prompt_digest
                && receipt.policy_digest == presentation.policy_digest
                && receipt.output_digest == presentation.output_digest
        }) {
            return invalid(
                "campaign private export presentation",
                &presentation.id,
                "selected presentation is missing its body-free receipt",
            );
        }
    }
    for asset in &exported.selected_generated_assets {
        super::validate_generated_asset_fields(
            &asset.id,
            &document.id,
            asset.turn_id.as_deref(),
            &asset.asset_kind,
            &asset.provider,
            &asset.model,
            &asset.location,
            &asset.metadata,
        )?;
        if asset
            .turn_id
            .as_deref()
            .is_some_and(|id| !turn_ids.contains(id))
        {
            return invalid(
                "campaign private export generated asset",
                &asset.id,
                "selected asset references an unknown turn",
            );
        }
    }
    Ok(())
}

fn canonical_json<T: Serialize>(value: &T) -> Result<String, RepositoryError> {
    let value = serde_json::to_value(value).map_err(|source| RepositoryError::Serialize {
        entity: "canonical JSON",
        source,
    })?;
    serde_json::to_string(&canonicalize_value(value)).map_err(|source| RepositoryError::Serialize {
        entity: "canonical JSON",
        source,
    })
}

fn canonicalize_value(value: Value) -> Value {
    match value {
        Value::Object(map) => {
            let sorted = map
                .into_iter()
                .map(|(key, value)| (key, canonicalize_value(value)))
                .collect::<std::collections::BTreeMap<_, _>>();
            Value::Object(sorted.into_iter().collect())
        }
        Value::Array(values) => Value::Array(values.into_iter().map(canonicalize_value).collect()),
        scalar => scalar,
    }
}

fn command_fingerprint<T: Serialize>(
    command_kind: &str,
    command: &T,
) -> Result<Sha256Digest, RepositoryError> {
    let payload = canonical_json(command)?;
    Ok(digest(format!("{command_kind}\n{payload}").as_bytes()))
}

fn digest(bytes: &[u8]) -> Sha256Digest {
    let bytes: [u8; 32] = Sha256::digest(bytes).into();
    Sha256Digest::from_bytes(bytes)
}

fn render_player_export(exported: &CampaignPrivateExportV1) -> Result<String, RepositoryError> {
    exported.validate()?;
    let mut output = String::new();
    let campaign = &exported.campaign;
    let state = match campaign.lifecycle_state {
        CampaignLifecycleState::Active => "active",
        CampaignLifecycleState::Archived => "archived",
    };
    output.push_str("# Manchester Arcana private campaign record\n\n");
    output.push_str(&format!(
        "Campaign: {}\n\nStatus: {state}\n\nCampaign revision: {}\n\nExported: {}\n\n",
        campaign.document.value.title, campaign.document.revision, exported.exported_at
    ));
    output.push_str("## Character sheet\n\n");
    if let Some(hero) = &exported.hero_character {
        let sheet = serde_json::to_string_pretty(&hero.value).map_err(|source| {
            RepositoryError::Serialize {
                entity: "player-readable hero sheet",
                source,
            }
        })?;
        output.push_str("```json\n");
        output.push_str(&sheet);
        output.push_str("\n```\n\n");
    } else {
        for character in &exported.characters {
            output.push_str(&format!(
                "- {} — {}, level {}, {} XP, {}/{} HP\n",
                character.value.name(),
                character.value.theme(),
                character.value.level().value(),
                character.value.experience_points(),
                character.value.current_hit_points(),
                character.value.maximum_hit_points(),
            ));
        }
        output.push('\n');
    }
    output.push_str("## Committed history\n\n");
    if exported.turns.is_empty() {
        output.push_str("No committed turns yet.\n\n");
    }
    for turn in &exported.turns {
        let payload = serde_json::to_value(&turn.event.payload).map_err(|source| {
            RepositoryError::Serialize {
                entity: "player-readable turn",
                source,
            }
        })?;
        let kind = payload
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("committed_event");
        output.push_str(&format!(
            "### Turn {} — {}\n\nRecorded: {}\n\n",
            turn.turn_number,
            kind.replace('_', " "),
            turn.created_at
        ));
        let mut rolls = Vec::new();
        collect_roll_facts(&payload, "event", &mut rolls);
        if !rolls.is_empty() {
            output.push_str("Stored dice and rule facts:\n\n");
            for roll in rolls {
                output.push_str("- ");
                output.push_str(&roll);
                output.push('\n');
            }
            output.push('\n');
        }
        let pretty = serde_json::to_string_pretty(&payload).map_err(|source| {
            RepositoryError::Serialize {
                entity: "player-readable turn",
                source,
            }
        })?;
        output.push_str("Stored audit facts:\n\n```json\n");
        output.push_str(&pretty);
        output.push_str("\n```\n\n");
        if let Some(presentation) = exported
            .selected_text_presentations
            .iter()
            .find(|presentation| presentation.origin_turn_id == turn.id)
        {
            output.push_str("Selected narration:\n\n");
            output.push_str(&presentation.body);
            output.push_str("\n\nProvenance: ");
            output.push_str(&format!(
                "{}; output {}; prompt {}; policy {}; config {}.\n\n",
                presentation.source,
                presentation.output_digest,
                presentation.prompt_digest,
                presentation.policy_digest,
                presentation.config_digest,
            ));
        }
    }
    output.push_str("## Selected generated artifacts\n\n");
    if exported.selected_generated_assets.is_empty() {
        output.push_str("No selected generated media.\n\n");
    } else {
        for asset in &exported.selected_generated_assets {
            output.push_str(&format!(
                "- {} — {} via {}/{}; protected key `{}`",
                asset.id, asset.asset_kind, asset.provider, asset.model, asset.location
            ));
            if let Some(fingerprint) = &asset.prompt_fingerprint {
                output.push_str(&format!("; prompt {fingerprint}"));
            }
            output.push_str(".\n");
        }
        output.push('\n');
    }
    output.push_str("## Durable private recap\n\n");
    if let Some(recap) = exported.private_recaps.last() {
        output.push_str(&recap.body);
        output.push_str("\n\nRecap provenance: ");
        output.push_str(&format!(
            "{}; source {}; body {}.\n\n",
            recap.template_id, recap.source_audit_digest, recap.body_digest
        ));
    } else {
        output.push_str("No durable private recap has been created.\n\n");
    }
    output.push_str("## Rules, content, and provenance pins\n\n");
    if let Some(pins) = &exported.content_pins {
        let pretty = serde_json::to_string_pretty(&pins.evidence).map_err(|source| {
            RepositoryError::Serialize {
                entity: "player-readable campaign pins",
                source,
            }
        })?;
        output.push_str("```json\n");
        output.push_str(&pretty);
        output.push_str("\n```\n\n");
    } else {
        output.push_str("Campaign setup is not sealed yet.\n\n");
    }
    output.push_str("## Attribution\n\n");
    output.push_str(
        "Rules-derived material uses the Dungeons & Dragons 5.1 SRD under CC BY 4.0. Manchester Arcana campaign content and its exact source identities are recorded in the pins above. This is a private owner export, not a public share artifact.\n",
    );
    if output.len() > MAX_PLAYER_EXPORT_BYTES {
        return invalid(
            "player-readable campaign export",
            &campaign.document.id,
            "rendered export exceeds the private export limit",
        );
    }
    Ok(output)
}

fn collect_roll_facts(value: &Value, path: &str, facts: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                let child_path = format!("{path}.{key}");
                if (key.contains("roll") || key == "total" || key == "modifier" || key == "dc")
                    && !matches!(child, Value::Object(_) | Value::Array(_))
                {
                    facts.push(format!("{child_path} = {child}"));
                }
                collect_roll_facts(child, &child_path, facts);
            }
        }
        Value::Array(values) => {
            for (index, child) in values.iter().enumerate() {
                collect_roll_facts(child, &format!("{path}[{index}]"), facts);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

fn validate_owner_key(owner_key: &str) -> Result<(), RepositoryError> {
    if !is_valid_opaque_id(owner_key) {
        return invalid(
            "campaign owner",
            "owner-scoped",
            "owner key must be a valid opaque identifier",
        );
    }
    Ok(())
}

fn validate_owner_campaign(
    owner_key: &str,
    campaign_session_id: &str,
) -> Result<(), RepositoryError> {
    validate_owner_key(owner_key)?;
    if !is_valid_opaque_id(campaign_session_id) {
        return invalid(
            "campaign session",
            "owner-scoped",
            "campaign id must be a valid opaque identifier",
        );
    }
    Ok(())
}

async fn require_owned_campaign(
    pool: &sqlx::PgPool,
    owner_key: &str,
    campaign_session_id: &str,
) -> Result<(), RepositoryError> {
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(
             SELECT 1 FROM campaign_sessions WHERE id = $1 AND owner_key = $2
         )",
    )
    .bind(campaign_session_id)
    .bind(owner_key)
    .fetch_one(pool)
    .await
    .map_err(RepositoryError::Database)?;
    if !exists {
        return Err(RepositoryError::NotFound {
            entity: "campaign session",
            id: campaign_session_id.to_owned(),
        });
    }
    Ok(())
}

fn map_lifecycle_insert(error: sqlx::Error, entity: &'static str, id: &str) -> RepositoryError {
    if error
        .as_database_error()
        .is_some_and(|database_error| database_error.is_unique_violation())
    {
        RepositoryError::AlreadyExists {
            entity,
            id: id.to_owned(),
        }
    } else {
        RepositoryError::Database(error)
    }
}

fn invalid<T>(entity: &'static str, id: &str, reason: &'static str) -> Result<T, RepositoryError> {
    Err(RepositoryError::InvalidDomainState {
        entity,
        id: id.to_owned(),
        reason,
    })
}

impl PostgresRepository {
    pub async fn start_campaign_play_session(
        &self,
        owner_key: &str,
        command: &StartPlaySessionCommand,
    ) -> Result<CampaignLifecycleOutcome, RepositoryError> {
        command.lifecycle.validate()?;
        validate_owner_key(owner_key)?;
        if !is_valid_opaque_id(&command.play_session_id) {
            return invalid(
                "play session command",
                &command.play_session_id,
                "play session id must be a valid opaque identifier",
            );
        }
        let request_fingerprint = command_fingerprint("play_start", command)?;
        let mut transaction = self.pool.begin().await.map_err(RepositoryError::Database)?;
        if let Some(outcome) = replay_lifecycle_receipt(
            &mut transaction,
            owner_key,
            &command.lifecycle,
            "play_start",
            &request_fingerprint,
        )
        .await?
        {
            transaction
                .commit()
                .await
                .map_err(RepositoryError::Database)?;
            return Ok(outcome);
        }
        let locked = lock_owned_campaign(
            &mut transaction,
            owner_key,
            &command.lifecycle.campaign_session_id,
        )
        .await?;
        locked.require_revision(command.lifecycle.expected_lifecycle_revision)?;
        locked.require_state(CampaignLifecycleState::Active)?;
        let existing_open: Option<String> = sqlx::query_scalar(
            "SELECT id FROM campaign_play_sessions
             WHERE campaign_session_id = $1 AND state IN ('waiting', 'active') FOR UPDATE",
        )
        .bind(&command.lifecycle.campaign_session_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(RepositoryError::Database)?;
        if existing_open.is_some() {
            return invalid(
                "play session",
                &command.lifecycle.campaign_session_id,
                "campaign already has an open play session",
            );
        }
        sqlx::query(
            "INSERT INTO campaign_play_sessions
             (id, campaign_session_id, owner_key, schema_version, state,
              started_campaign_revision)
             VALUES ($1, $2, $3, $4, 'waiting', $5)",
        )
        .bind(&command.play_session_id)
        .bind(&command.lifecycle.campaign_session_id)
        .bind(owner_key)
        .bind(i64::from(CAMPAIGN_LIFECYCLE_SCHEMA_VERSION))
        .bind(to_i64(locked.campaign_revision, "campaign revision")?)
        .execute(&mut *transaction)
        .await
        .map_err(|error| map_lifecycle_insert(error, "play session", &command.play_session_id))?;
        let outcome = advance_lifecycle(
            &mut transaction,
            owner_key,
            &command.lifecycle,
            locked,
            CampaignLifecycleState::Active,
            LifecycleAuditPayload::PlayStarted {
                play_session_id: command.play_session_id.clone(),
            },
            Some(command.play_session_id.clone()),
            false,
        )
        .await?;
        insert_lifecycle_receipt(
            &mut transaction,
            owner_key,
            &command.lifecycle,
            "play_start",
            &request_fingerprint,
            &outcome,
        )
        .await?;
        transaction
            .commit()
            .await
            .map_err(RepositoryError::Database)?;
        Ok(outcome)
    }

    pub async fn end_campaign_play_session(
        &self,
        owner_key: &str,
        command: &EndPlaySessionCommand,
    ) -> Result<CampaignLifecycleOutcome, RepositoryError> {
        command.lifecycle.validate()?;
        validate_owner_key(owner_key)?;
        if !is_valid_opaque_id(&command.play_session_id) {
            return invalid(
                "play session command",
                &command.play_session_id,
                "play session id must be a valid opaque identifier",
            );
        }
        let request_fingerprint = command_fingerprint("play_end", command)?;
        let mut transaction = self.pool.begin().await.map_err(RepositoryError::Database)?;
        if let Some(outcome) = replay_lifecycle_receipt(
            &mut transaction,
            owner_key,
            &command.lifecycle,
            "play_end",
            &request_fingerprint,
        )
        .await?
        {
            transaction
                .commit()
                .await
                .map_err(RepositoryError::Database)?;
            return Ok(outcome);
        }
        let locked = lock_owned_campaign(
            &mut transaction,
            owner_key,
            &command.lifecycle.campaign_session_id,
        )
        .await?;
        locked.require_revision(command.lifecycle.expected_lifecycle_revision)?;
        locked.require_state(CampaignLifecycleState::Active)?;
        let updated = sqlx::query(
            "UPDATE campaign_play_sessions
             SET state = 'closed', ended_campaign_revision = $4,
                 closed_at = CURRENT_TIMESTAMP, close_reason = 'owner_ended'
             WHERE id = $1 AND campaign_session_id = $2 AND owner_key = $3
               AND state IN ('waiting', 'active')",
        )
        .bind(&command.play_session_id)
        .bind(&command.lifecycle.campaign_session_id)
        .bind(owner_key)
        .bind(to_i64(locked.campaign_revision, "campaign revision")?)
        .execute(&mut *transaction)
        .await
        .map_err(RepositoryError::Database)?;
        if updated.rows_affected() != 1 {
            return invalid(
                "play session",
                &command.play_session_id,
                "the requested play session is not open for this campaign",
            );
        }
        let outcome = advance_lifecycle(
            &mut transaction,
            owner_key,
            &command.lifecycle,
            locked,
            CampaignLifecycleState::Active,
            LifecycleAuditPayload::PlayEnded {
                play_session_id: command.play_session_id.clone(),
            },
            Some(command.play_session_id.clone()),
            false,
        )
        .await?;
        insert_lifecycle_receipt(
            &mut transaction,
            owner_key,
            &command.lifecycle,
            "play_end",
            &request_fingerprint,
            &outcome,
        )
        .await?;
        transaction
            .commit()
            .await
            .map_err(RepositoryError::Database)?;
        Ok(outcome)
    }

    pub async fn archive_campaign(
        &self,
        owner_key: &str,
        command: &CampaignLifecycleCommand,
    ) -> Result<CampaignLifecycleOutcome, RepositoryError> {
        self.change_archive_state(owner_key, command, true).await
    }

    pub async fn restore_archived_campaign(
        &self,
        owner_key: &str,
        command: &CampaignLifecycleCommand,
    ) -> Result<CampaignLifecycleOutcome, RepositoryError> {
        self.change_archive_state(owner_key, command, false).await
    }

    async fn change_archive_state(
        &self,
        owner_key: &str,
        command: &CampaignLifecycleCommand,
        archive: bool,
    ) -> Result<CampaignLifecycleOutcome, RepositoryError> {
        command.validate()?;
        validate_owner_key(owner_key)?;
        let (command_kind, required_state, next_state, payload) = if archive {
            (
                "archive",
                CampaignLifecycleState::Active,
                CampaignLifecycleState::Archived,
                LifecycleAuditPayload::Archived,
            )
        } else {
            (
                "restore_archive",
                CampaignLifecycleState::Archived,
                CampaignLifecycleState::Active,
                LifecycleAuditPayload::Restored,
            )
        };
        let request_fingerprint = command_fingerprint(command_kind, command)?;
        let mut transaction = self.pool.begin().await.map_err(RepositoryError::Database)?;
        if let Some(outcome) = replay_lifecycle_receipt(
            &mut transaction,
            owner_key,
            command,
            command_kind,
            &request_fingerprint,
        )
        .await?
        {
            transaction
                .commit()
                .await
                .map_err(RepositoryError::Database)?;
            return Ok(outcome);
        }
        let locked =
            lock_owned_campaign(&mut transaction, owner_key, &command.campaign_session_id).await?;
        locked.require_revision(command.expected_lifecycle_revision)?;
        locked.require_state(required_state)?;
        if archive {
            require_no_open_play_session(&mut transaction, &command.campaign_session_id).await?;
        }
        let outcome = advance_lifecycle(
            &mut transaction,
            owner_key,
            command,
            locked,
            next_state,
            payload,
            None,
            false,
        )
        .await?;
        insert_lifecycle_receipt(
            &mut transaction,
            owner_key,
            command,
            command_kind,
            &request_fingerprint,
            &outcome,
        )
        .await?;
        transaction
            .commit()
            .await
            .map_err(RepositoryError::Database)?;
        Ok(outcome)
    }

    pub async fn delete_archived_campaign(
        &self,
        owner_key: &str,
        command: &DeleteCampaignCommand,
    ) -> Result<CampaignLifecycleOutcome, RepositoryError> {
        command.lifecycle.validate()?;
        validate_owner_key(owner_key)?;
        if !is_valid_opaque_id(&command.deletion_id) || !command.confirm_permanent_delete {
            return invalid(
                "campaign delete command",
                &command.deletion_id,
                "deletion id and explicit permanent-delete confirmation are required",
            );
        }
        let request_fingerprint = command_fingerprint("delete", command)?;
        let mut transaction = self.pool.begin().await.map_err(RepositoryError::Database)?;
        if let Some(outcome) = replay_lifecycle_receipt(
            &mut transaction,
            owner_key,
            &command.lifecycle,
            "delete",
            &request_fingerprint,
        )
        .await?
        {
            transaction
                .commit()
                .await
                .map_err(RepositoryError::Database)?;
            return Ok(outcome);
        }
        let locked = lock_owned_campaign(
            &mut transaction,
            owner_key,
            &command.lifecycle.campaign_session_id,
        )
        .await?;
        locked.require_revision(command.lifecycle.expected_lifecycle_revision)?;
        locked.require_state(CampaignLifecycleState::Archived)?;
        require_no_open_play_session(&mut transaction, &command.lifecycle.campaign_session_id)
            .await?;
        let preparation = sqlx::query(
            "SELECT campaign_revision, lifecycle_revision, canonical_export_digest
             FROM campaign_deletion_preparations
             WHERE owner_key = $1 AND campaign_session_id = $2 AND deletion_id = $3
               AND expires_at > CURRENT_TIMESTAMP
             FOR UPDATE",
        )
        .bind(owner_key)
        .bind(&command.lifecycle.campaign_session_id)
        .bind(&command.deletion_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(RepositoryError::Database)?
        .ok_or_else(|| RepositoryError::NotFound {
            entity: "campaign deletion preparation",
            id: command.deletion_id.clone(),
        })?;
        let prepared_campaign_revision = from_i64(
            preparation
                .try_get("campaign_revision")
                .map_err(RepositoryError::Database)?,
            "prepared campaign revision",
        )?;
        let prepared_lifecycle_revision = from_i64(
            preparation
                .try_get("lifecycle_revision")
                .map_err(RepositoryError::Database)?,
            "prepared lifecycle revision",
        )?;
        if prepared_campaign_revision != locked.campaign_revision
            || prepared_lifecycle_revision != locked.lifecycle_revision
        {
            return invalid(
                "campaign deletion preparation",
                &command.deletion_id,
                "campaign changed after its delete export was prepared",
            );
        }
        let prepared_export_digest: String = preparation
            .try_get("canonical_export_digest")
            .map_err(RepositoryError::Database)?;
        let result_revision = command
            .lifecycle
            .expected_lifecycle_revision
            .checked_add(1)
            .ok_or(RepositoryError::NumericRange {
                field: "lifecycle revision",
            })?;
        let outcome = CampaignLifecycleOutcome {
            schema_version: CAMPAIGN_LIFECYCLE_SCHEMA_VERSION,
            campaign_session_id: command.lifecycle.campaign_session_id.clone(),
            lifecycle_revision: result_revision,
            lifecycle_state: None,
            play_session_id: None,
            deleted: true,
        };
        insert_lifecycle_receipt(
            &mut transaction,
            owner_key,
            &command.lifecycle,
            "delete",
            &request_fingerprint,
            &outcome,
        )
        .await?;
        sqlx::query(
            "INSERT INTO campaign_deletion_tombstones
             (owner_key, campaign_session_id, deletion_id,
              deleted_lifecycle_revision, canonical_export_digest)
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(owner_key)
        .bind(&command.lifecycle.campaign_session_id)
        .bind(&command.deletion_id)
        .bind(to_i64(result_revision, "lifecycle revision")?)
        .bind(prepared_export_digest)
        .execute(&mut *transaction)
        .await
        .map_err(|error| {
            map_lifecycle_insert(error, "campaign deletion tombstone", &command.deletion_id)
        })?;
        let deleted = sqlx::query("DELETE FROM campaign_sessions WHERE id = $1 AND owner_key = $2")
            .bind(&command.lifecycle.campaign_session_id)
            .bind(owner_key)
            .execute(&mut *transaction)
            .await
            .map_err(RepositoryError::Database)?;
        if deleted.rows_affected() != 1 {
            return Err(RepositoryError::NotFound {
                entity: "campaign session",
                id: command.lifecycle.campaign_session_id.clone(),
            });
        }
        transaction
            .commit()
            .await
            .map_err(RepositoryError::Database)?;
        Ok(outcome)
    }

    pub async fn delete_expired_campaign_lifecycle_metadata(
        &self,
        limit: u16,
    ) -> Result<(u64, u64, u64), RepositoryError> {
        if limit == 0 || limit > 1_000 {
            return invalid(
                "campaign lifecycle cleanup",
                "retention",
                "cleanup limit must be between one and one thousand",
            );
        }
        let mut transaction = self.pool.begin().await.map_err(RepositoryError::Database)?;
        let preparations = sqlx::query(
            "WITH expired AS (
                SELECT owner_key, campaign_session_id, deletion_id
                FROM campaign_deletion_preparations
                WHERE expires_at <= CURRENT_TIMESTAMP
                ORDER BY expires_at, owner_key, campaign_session_id, deletion_id
                LIMIT $1 FOR UPDATE SKIP LOCKED
             )
             DELETE FROM campaign_deletion_preparations p
             USING expired e
             WHERE p.owner_key = e.owner_key
               AND p.campaign_session_id = e.campaign_session_id
               AND p.deletion_id = e.deletion_id",
        )
        .bind(i64::from(limit))
        .execute(&mut *transaction)
        .await
        .map_err(RepositoryError::Database)?
        .rows_affected();
        let receipts = sqlx::query(
            "WITH expired AS (
                SELECT owner_key, campaign_session_id, idempotency_key
                FROM campaign_lifecycle_receipts
                WHERE retention_delete_after <= CURRENT_TIMESTAMP
                ORDER BY retention_delete_after, owner_key, campaign_session_id, idempotency_key
                LIMIT $1 FOR UPDATE SKIP LOCKED
             )
             DELETE FROM campaign_lifecycle_receipts r
             USING expired e
             WHERE r.owner_key = e.owner_key
               AND r.campaign_session_id = e.campaign_session_id
               AND r.idempotency_key = e.idempotency_key",
        )
        .bind(i64::from(limit))
        .execute(&mut *transaction)
        .await
        .map_err(RepositoryError::Database)?
        .rows_affected();
        let tombstones = sqlx::query(
            "WITH expired AS (
                SELECT owner_key, campaign_session_id, deletion_id
                FROM campaign_deletion_tombstones
                WHERE retention_delete_after <= CURRENT_TIMESTAMP
                ORDER BY retention_delete_after, owner_key, campaign_session_id
                LIMIT $1 FOR UPDATE SKIP LOCKED
             )
             DELETE FROM campaign_deletion_tombstones t
             USING expired e
             WHERE t.owner_key = e.owner_key
               AND t.campaign_session_id = e.campaign_session_id
               AND t.deletion_id = e.deletion_id",
        )
        .bind(i64::from(limit))
        .execute(&mut *transaction)
        .await
        .map_err(RepositoryError::Database)?
        .rows_affected();
        transaction
            .commit()
            .await
            .map_err(RepositoryError::Database)?;
        Ok((preparations, receipts, tombstones))
    }
}

#[derive(Debug, Clone, Copy)]
struct LockedCampaign {
    campaign_revision: u64,
    lifecycle_revision: u64,
    lifecycle_state: CampaignLifecycleState,
}

impl LockedCampaign {
    fn require_revision(self, expected: u64) -> Result<(), RepositoryError> {
        if self.lifecycle_revision != expected {
            return Err(RepositoryError::RevisionConflict {
                entity: "campaign lifecycle",
                id: "owner-scoped-campaign".to_owned(),
                expected,
                actual: self.lifecycle_revision,
            });
        }
        Ok(())
    }

    fn require_state(self, expected: CampaignLifecycleState) -> Result<(), RepositoryError> {
        if self.lifecycle_state != expected {
            return invalid(
                "campaign lifecycle",
                "owner-scoped-campaign",
                match expected {
                    CampaignLifecycleState::Active => "campaign must be active",
                    CampaignLifecycleState::Archived => "campaign must be archived",
                },
            );
        }
        Ok(())
    }
}

async fn lock_owned_campaign(
    transaction: &mut Transaction<'_, Postgres>,
    owner_key: &str,
    campaign_session_id: &str,
) -> Result<LockedCampaign, RepositoryError> {
    let row = sqlx::query(
        "SELECT revision, lifecycle_revision, lifecycle_state
         FROM campaign_sessions
         WHERE id = $1 AND owner_key = $2
         FOR UPDATE",
    )
    .bind(campaign_session_id)
    .bind(owner_key)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(RepositoryError::Database)?
    .ok_or_else(|| RepositoryError::NotFound {
        entity: "campaign session",
        id: campaign_session_id.to_owned(),
    })?;
    let state: String = row
        .try_get("lifecycle_state")
        .map_err(RepositoryError::Database)?;
    Ok(LockedCampaign {
        campaign_revision: from_i64(
            row.try_get("revision").map_err(RepositoryError::Database)?,
            "campaign revision",
        )?,
        lifecycle_revision: from_i64(
            row.try_get("lifecycle_revision")
                .map_err(RepositoryError::Database)?,
            "lifecycle revision",
        )?,
        lifecycle_state: CampaignLifecycleState::try_from(state.as_str())?,
    })
}

async fn require_no_open_play_session(
    transaction: &mut Transaction<'_, Postgres>,
    campaign_session_id: &str,
) -> Result<(), RepositoryError> {
    let open: Option<String> = sqlx::query_scalar(
        "SELECT id FROM campaign_play_sessions
         WHERE campaign_session_id = $1 AND state IN ('waiting', 'active') FOR UPDATE",
    )
    .bind(campaign_session_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(RepositoryError::Database)?;
    if open.is_some() {
        return invalid(
            "campaign lifecycle",
            campaign_session_id,
            "end the open play session before this lifecycle change",
        );
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn advance_lifecycle(
    transaction: &mut Transaction<'_, Postgres>,
    owner_key: &str,
    command: &CampaignLifecycleCommand,
    locked: LockedCampaign,
    next_state: CampaignLifecycleState,
    payload: LifecycleAuditPayload,
    play_session_id: Option<String>,
    deleted: bool,
) -> Result<CampaignLifecycleOutcome, RepositoryError> {
    if !payload.validate() {
        return invalid(
            "campaign lifecycle audit",
            &command.campaign_session_id,
            "audit payload is invalid",
        );
    }
    let next_revision =
        locked
            .lifecycle_revision
            .checked_add(1)
            .ok_or(RepositoryError::NumericRange {
                field: "lifecycle revision",
            })?;
    let updated = match next_state {
        CampaignLifecycleState::Active => sqlx::query(
            "UPDATE campaign_sessions
                 SET lifecycle_revision = $3, lifecycle_state = 'active',
                     archived_at = NULL, retention_class = 'campaign_lifetime',
                     retention_delete_after = NULL, updated_at = CURRENT_TIMESTAMP
                 WHERE id = $1 AND owner_key = $2 AND lifecycle_revision = $4",
        )
        .bind(&command.campaign_session_id)
        .bind(owner_key)
        .bind(to_i64(next_revision, "lifecycle revision")?)
        .bind(to_i64(locked.lifecycle_revision, "lifecycle revision")?)
        .execute(&mut **transaction)
        .await
        .map_err(RepositoryError::Database)?,
        CampaignLifecycleState::Archived => sqlx::query(
            "UPDATE campaign_sessions
                 SET lifecycle_revision = $3, lifecycle_state = 'archived',
                     archived_at = CURRENT_TIMESTAMP,
                     retention_class = 'archived_owner_managed',
                     retention_delete_after = NULL, updated_at = CURRENT_TIMESTAMP
                 WHERE id = $1 AND owner_key = $2 AND lifecycle_revision = $4",
        )
        .bind(&command.campaign_session_id)
        .bind(owner_key)
        .bind(to_i64(next_revision, "lifecycle revision")?)
        .bind(to_i64(locked.lifecycle_revision, "lifecycle revision")?)
        .execute(&mut **transaction)
        .await
        .map_err(RepositoryError::Database)?,
    };
    if updated.rows_affected() != 1 {
        return Err(RepositoryError::RevisionConflict {
            entity: "campaign lifecycle",
            id: command.campaign_session_id.clone(),
            expected: locked.lifecycle_revision,
            actual: locked.lifecycle_revision.saturating_add(1),
        });
    }
    let audit_id = format!("lifecycle-{}", uuid::Uuid::new_v4());
    let payload_json = serialize("campaign lifecycle audit", &payload)?;
    sqlx::query(
        "INSERT INTO campaign_lifecycle_audits
         (id, campaign_session_id, owner_key, schema_version,
          lifecycle_revision, event_kind, payload_json)
         VALUES ($1, $2, $3, $4, $5, $6, $7::jsonb)",
    )
    .bind(&audit_id)
    .bind(&command.campaign_session_id)
    .bind(owner_key)
    .bind(i64::from(CAMPAIGN_LIFECYCLE_SCHEMA_VERSION))
    .bind(to_i64(next_revision, "lifecycle revision")?)
    .bind(payload.event_kind())
    .bind(payload_json)
    .execute(&mut **transaction)
    .await
    .map_err(|error| map_lifecycle_insert(error, "campaign lifecycle audit", &audit_id))?;
    Ok(CampaignLifecycleOutcome {
        schema_version: CAMPAIGN_LIFECYCLE_SCHEMA_VERSION,
        campaign_session_id: command.campaign_session_id.clone(),
        lifecycle_revision: next_revision,
        lifecycle_state: Some(next_state),
        play_session_id,
        deleted,
    })
}

async fn replay_lifecycle_receipt(
    transaction: &mut Transaction<'_, Postgres>,
    owner_key: &str,
    command: &CampaignLifecycleCommand,
    command_kind: &str,
    request_fingerprint: &Sha256Digest,
) -> Result<Option<CampaignLifecycleOutcome>, RepositoryError> {
    let row = sqlx::query(
        "SELECT command_kind, request_fingerprint, expected_lifecycle_revision,
                response_json
         FROM campaign_lifecycle_receipts
         WHERE owner_key = $1 AND campaign_session_id = $2 AND idempotency_key = $3
         FOR UPDATE",
    )
    .bind(owner_key)
    .bind(&command.campaign_session_id)
    .bind(&command.idempotency_key)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(RepositoryError::Database)?;
    let Some(row) = row else {
        return Ok(None);
    };
    let stored_kind: String = row
        .try_get("command_kind")
        .map_err(RepositoryError::Database)?;
    let stored_fingerprint: String = row
        .try_get("request_fingerprint")
        .map_err(RepositoryError::Database)?;
    let stored_expected = from_i64(
        row.try_get("expected_lifecycle_revision")
            .map_err(RepositoryError::Database)?,
        "expected lifecycle revision",
    )?;
    if stored_kind != command_kind
        || stored_fingerprint != request_fingerprint.as_str()
        || stored_expected != command.expected_lifecycle_revision
    {
        return invalid(
            "campaign lifecycle receipt",
            &command.idempotency_key,
            "idempotency key was reused for a different command",
        );
    }
    let response_json: String = row
        .try_get("response_json")
        .map_err(RepositoryError::Database)?;
    let outcome = serde_json::from_str(&response_json).map_err(|source| {
        RepositoryError::InvalidStoredData {
            entity: "campaign lifecycle receipt",
            id: command.idempotency_key.clone(),
            source,
        }
    })?;
    Ok(Some(outcome))
}

async fn insert_lifecycle_receipt(
    transaction: &mut Transaction<'_, Postgres>,
    owner_key: &str,
    command: &CampaignLifecycleCommand,
    command_kind: &str,
    request_fingerprint: &Sha256Digest,
    outcome: &CampaignLifecycleOutcome,
) -> Result<(), RepositoryError> {
    let response_json = serialize("campaign lifecycle response", outcome)?;
    sqlx::query(
        "INSERT INTO campaign_lifecycle_receipts
         (owner_key, campaign_session_id, idempotency_key, command_kind,
          request_fingerprint, expected_lifecycle_revision,
          result_lifecycle_revision, response_json)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
    )
    .bind(owner_key)
    .bind(&command.campaign_session_id)
    .bind(&command.idempotency_key)
    .bind(command_kind)
    .bind(request_fingerprint.as_str())
    .bind(to_i64(
        command.expected_lifecycle_revision,
        "expected lifecycle revision",
    )?)
    .bind(to_i64(
        outcome.lifecycle_revision,
        "result lifecycle revision",
    )?)
    .bind(response_json)
    .execute(&mut **transaction)
    .await
    .map_err(|error| {
        map_lifecycle_insert(
            error,
            "campaign lifecycle receipt",
            &command.idempotency_key,
        )
    })?;
    Ok(())
}

impl PostgresRepository {
    /// Creates a consistent private snapshot without selecting operational
    /// attempts. Only owner-selected presentation/artifact provenance is read.
    pub async fn export_campaign_private(
        &self,
        owner_key: &str,
        campaign_session_id: &str,
    ) -> Result<CampaignPrivateExportV1, RepositoryError> {
        validate_owner_campaign(owner_key, campaign_session_id)?;
        let mut transaction = self.pool.begin().await.map_err(RepositoryError::Database)?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
            .execute(&mut *transaction)
            .await
            .map_err(RepositoryError::Database)?;
        let campaign_row = sqlx::query(
            "SELECT id, schema_version, revision, payload_json::text AS payload_json,
                    owner_key, lifecycle_revision, lifecycle_state,
                    archived_at::text AS archived_at, safety_policy_id,
                    progression_policy_id, retention_class,
                    retention_delete_after::text AS retention_delete_after,
                    created_at::text AS created_at, updated_at::text AS updated_at
             FROM campaign_sessions
             WHERE id = $1 AND owner_key = $2",
        )
        .bind(campaign_session_id)
        .bind(owner_key)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(RepositoryError::Database)?
        .ok_or_else(|| RepositoryError::NotFound {
            entity: "campaign session",
            id: campaign_session_id.to_owned(),
        })?;
        let campaign = exported_campaign_from_row(campaign_row)?;
        let exported_at: String = sqlx::query_scalar("SELECT CURRENT_TIMESTAMP::text")
            .fetch_one(&mut *transaction)
            .await
            .map_err(RepositoryError::Database)?;

        let character_rows = sqlx::query(
            "SELECT id, schema_version, revision, payload_json::text AS payload_json,
                    created_at::text AS created_at, updated_at::text AS updated_at
             FROM characters WHERE campaign_session_id = $1 ORDER BY id",
        )
        .bind(campaign_session_id)
        .fetch_all(&mut *transaction)
        .await
        .map_err(RepositoryError::Database)?;
        let characters = character_rows
            .into_iter()
            .map(|row| exported_document_from_row(&row, "character"))
            .collect::<Result<Vec<ExportedDocument<Character>>, _>>()?;

        let draft_rows = sqlx::query(
            "SELECT id, schema_version, revision, payload_json::text AS payload_json,
                    expires_at_epoch_seconds, retention_delete_after_epoch_seconds,
                    created_at::text AS created_at, updated_at::text AS updated_at
             FROM hero_creation_drafts
             WHERE campaign_session_id = $1 AND owner_key = $2
             ORDER BY created_at, id",
        )
        .bind(campaign_session_id)
        .bind(owner_key)
        .fetch_all(&mut *transaction)
        .await
        .map_err(RepositoryError::Database)?;
        let hero_drafts = draft_rows
            .into_iter()
            .map(exported_hero_draft_from_row)
            .collect::<Result<Vec<_>, _>>()?;

        let hero_row = sqlx::query(
            "SELECT id, schema_version, revision, payload_json::text AS payload_json,
                    created_at::text AS created_at, updated_at::text AS updated_at
             FROM hero_characters
             WHERE campaign_session_id = $1 AND owner_key = $2",
        )
        .bind(campaign_session_id)
        .bind(owner_key)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(RepositoryError::Database)?;
        let hero_character = hero_row
            .map(|row| exported_document_from_row(&row, "hero character"))
            .transpose()?;

        let turn_rows = sqlx::query(
            "SELECT id, turn_number, actor_id, correlation_id, schema_version,
                    payload_json::text AS payload_json, created_at::text AS created_at
             FROM turn_audits WHERE campaign_session_id = $1
             ORDER BY turn_number, id",
        )
        .bind(campaign_session_id)
        .fetch_all(&mut *transaction)
        .await
        .map_err(RepositoryError::Database)?;
        let turns = turn_rows
            .into_iter()
            .map(exported_turn_from_row)
            .collect::<Result<Vec<_>, _>>()?;

        let receipt_rows = sqlx::query(
            "SELECT idempotency_key, command_kind, request_fingerprint,
                    expected_revision, result_revision, audit_id, response_json,
                    created_at::text AS created_at
             FROM command_receipts WHERE campaign_session_id = $1
             ORDER BY created_at, idempotency_key",
        )
        .bind(campaign_session_id)
        .fetch_all(&mut *transaction)
        .await
        .map_err(RepositoryError::Database)?;
        let command_receipts = receipt_rows
            .into_iter()
            .map(exported_command_receipt_from_row)
            .collect::<Result<Vec<_>, _>>()?;

        let text_receipt_rows = sqlx::query(
            "SELECT schema_version, origin_turn_id, client_idempotency_key,
                    presentation_id, generation_job_id, generation_attempt_id,
                    version, source, config_digest, prompt_digest, policy_digest,
                    output_digest, created_at::text AS created_at
             FROM generated_text_presentation_receipts
             WHERE campaign_session_id = $1
             ORDER BY origin_turn_id, version, client_idempotency_key",
        )
        .bind(campaign_session_id)
        .fetch_all(&mut *transaction)
        .await
        .map_err(RepositoryError::Database)?;
        let text_presentation_receipts = text_receipt_rows
            .into_iter()
            .map(exported_text_presentation_receipt_from_row)
            .collect::<Result<Vec<_>, _>>()?;

        let typed_receipt_rows = sqlx::query(
            "SELECT schema_version, client_idempotency_key, player_intent_digest,
                    expected_campaign_revision, expected_encounter_revision,
                    resolved_intent_json::text AS resolved_intent_json,
                    interpretation_label,
                    interpretation_evidence_json::text AS interpretation_evidence_json,
                    state, origin_turn_id, event_sequence, result_campaign_revision,
                    created_at::text AS created_at, updated_at::text AS updated_at
             FROM typed_intent_command_receipts
             WHERE campaign_session_id = $1
             ORDER BY created_at, client_idempotency_key",
        )
        .bind(campaign_session_id)
        .fetch_all(&mut *transaction)
        .await
        .map_err(RepositoryError::Database)?;
        let typed_intent_receipts = typed_receipt_rows
            .into_iter()
            .map(exported_typed_intent_receipt_from_row)
            .collect::<Result<Vec<_>, _>>()?;

        let hero_audit_rows = sqlx::query(
            "SELECT id, subject_kind, subject_id, audit_kind, schema_version,
                    subject_revision, occurred_at_epoch_seconds,
                    payload_json::text AS payload_json, created_at::text AS created_at
             FROM hero_audits WHERE campaign_session_id = $1
             ORDER BY occurred_at_epoch_seconds, subject_revision, id",
        )
        .bind(campaign_session_id)
        .fetch_all(&mut *transaction)
        .await
        .map_err(RepositoryError::Database)?;
        let hero_audits = hero_audit_rows
            .into_iter()
            .map(exported_hero_audit_from_row)
            .collect::<Result<Vec<_>, _>>()?;

        let hero_receipt_rows = sqlx::query(
            "SELECT scope_kind, scope_id, idempotency_key, command_kind,
                    request_fingerprint, expected_revision, result_revision,
                    audit_id, response_json, created_at::text AS created_at
             FROM hero_command_receipts WHERE campaign_session_id = $1
             ORDER BY created_at, scope_kind, scope_id, idempotency_key",
        )
        .bind(campaign_session_id)
        .fetch_all(&mut *transaction)
        .await
        .map_err(RepositoryError::Database)?;
        let hero_receipts = hero_receipt_rows
            .into_iter()
            .map(exported_hero_receipt_from_row)
            .collect::<Result<Vec<_>, _>>()?;

        let reward_rows = sqlx::query(
            "SELECT encounter_id, character_id, encounter_revision,
                    victory_event_sequence, reward_tier, experience_awarded,
                    hero_audit_id, created_at::text AS created_at
             FROM encounter_reward_claims WHERE campaign_session_id = $1
             ORDER BY created_at, encounter_id",
        )
        .bind(campaign_session_id)
        .fetch_all(&mut *transaction)
        .await
        .map_err(RepositoryError::Database)?;
        let encounter_reward_claims = reward_rows
            .into_iter()
            .map(exported_reward_claim_from_row)
            .collect::<Result<Vec<_>, _>>()?;

        let pins_row = sqlx::query(
            "SELECT seal_reason, payload_json::text AS payload_json,
                    legacy_source_json::text AS legacy_source_json,
                    created_at::text AS created_at
             FROM campaign_content_pins WHERE campaign_session_id = $1",
        )
        .bind(campaign_session_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(RepositoryError::Database)?;
        let content_pins = pins_row.map(exported_pins_from_row).transpose()?;

        let play_rows = sqlx::query(
            "SELECT id, campaign_session_id, owner_key, schema_version, state,
                    started_campaign_revision, ended_campaign_revision,
                    opened_at::text AS opened_at, closed_at::text AS closed_at,
                    close_reason
             FROM campaign_play_sessions
             WHERE campaign_session_id = $1 AND owner_key = $2
             ORDER BY opened_at, id",
        )
        .bind(campaign_session_id)
        .bind(owner_key)
        .fetch_all(&mut *transaction)
        .await
        .map_err(RepositoryError::Database)?;
        let play_sessions = play_rows
            .into_iter()
            .map(play_session_from_row)
            .collect::<Result<Vec<_>, _>>()?;

        let lifecycle_rows = sqlx::query(
            "SELECT id, lifecycle_revision, event_kind,
                    payload_json::text AS payload_json, created_at::text AS created_at
             FROM campaign_lifecycle_audits
             WHERE campaign_session_id = $1 AND owner_key = $2
             ORDER BY lifecycle_revision, id",
        )
        .bind(campaign_session_id)
        .bind(owner_key)
        .fetch_all(&mut *transaction)
        .await
        .map_err(RepositoryError::Database)?;
        let lifecycle_audits = lifecycle_rows
            .into_iter()
            .map(exported_lifecycle_audit_from_row)
            .collect::<Result<Vec<_>, _>>()?;

        let recap_rows = sqlx::query(
            "SELECT id, campaign_session_id, schema_version, campaign_revision,
                    idempotency_key, request_fingerprint, first_turn_number,
                    last_turn_number, source_audit_count, source_audit_digest,
                    template_id, body, body_digest, created_at::text AS created_at
             FROM campaign_private_recaps
             WHERE campaign_session_id = $1 AND owner_key = $2
             ORDER BY campaign_revision, created_at, id",
        )
        .bind(campaign_session_id)
        .bind(owner_key)
        .fetch_all(&mut *transaction)
        .await
        .map_err(RepositoryError::Database)?;
        let private_recaps = recap_rows
            .into_iter()
            .map(private_recap_from_row)
            .collect::<Result<Vec<_>, _>>()?;

        let presentation_rows = sqlx::query(
            "SELECT id, origin_turn_id, generation_job_id, generation_attempt_id,
                    client_idempotency_key, version, source, body, config_digest, prompt_digest,
                    policy_digest, output_digest,
                    created_at::text AS created_at, updated_at::text AS updated_at
             FROM generated_text_presentations
             WHERE campaign_session_id = $1 AND selected
             ORDER BY origin_turn_id, version, id",
        )
        .bind(campaign_session_id)
        .fetch_all(&mut *transaction)
        .await
        .map_err(RepositoryError::Database)?;
        let selected_text_presentations = presentation_rows
            .into_iter()
            .map(exported_presentation_from_row)
            .collect::<Result<Vec<_>, _>>()?;

        let asset_rows = sqlx::query(
            "SELECT a.id, a.turn_id, a.asset_kind, a.provider, a.model,
                    a.location, a.prompt_fingerprint,
                    a.metadata_json::text AS metadata_json,
                    a.created_at::text AS created_at
             FROM generated_assets a
             WHERE a.campaign_session_id = $1
               AND EXISTS (
                   SELECT 1 FROM generation_jobs j
                   WHERE j.artifact_id = a.id
                     AND j.campaign_session_id = a.campaign_session_id
                     AND j.state = 'succeeded'
                     AND j.retention_class = 'campaign_lifetime'
               )
             ORDER BY a.created_at, a.id",
        )
        .bind(campaign_session_id)
        .fetch_all(&mut *transaction)
        .await
        .map_err(RepositoryError::Database)?;
        let selected_generated_assets = asset_rows
            .into_iter()
            .map(exported_asset_from_row)
            .collect::<Result<Vec<_>, _>>()?;

        let exported = CampaignPrivateExportV1 {
            schema_version: CAMPAIGN_EXPORT_SCHEMA_VERSION,
            owner_key: owner_key.to_owned(),
            exported_at,
            campaign,
            characters,
            hero_drafts,
            hero_character,
            turns,
            command_receipts,
            text_presentation_receipts,
            typed_intent_receipts,
            hero_audits,
            hero_receipts,
            encounter_reward_claims,
            content_pins,
            play_sessions,
            lifecycle_audits,
            private_recaps,
            selected_text_presentations,
            selected_generated_assets,
        };
        exported.validate()?;
        transaction
            .commit()
            .await
            .map_err(RepositoryError::Database)?;
        Ok(exported)
    }

    pub async fn export_campaign_canonical_json(
        &self,
        owner_key: &str,
        campaign_session_id: &str,
    ) -> Result<String, RepositoryError> {
        self.export_campaign_private(owner_key, campaign_session_id)
            .await?
            .canonical_json()
    }

    pub async fn export_campaign_player_readable(
        &self,
        owner_key: &str,
        campaign_session_id: &str,
    ) -> Result<String, RepositoryError> {
        let exported = self
            .export_campaign_private(owner_key, campaign_session_id)
            .await?;
        render_player_export(&exported)
    }

    pub async fn prepare_campaign_deletion(
        &self,
        owner_key: &str,
        campaign_session_id: &str,
        expected_lifecycle_revision: u64,
        deletion_id: &str,
    ) -> Result<PreparedCampaignDeletion, RepositoryError> {
        validate_owner_campaign(owner_key, campaign_session_id)?;
        if expected_lifecycle_revision == 0 || !is_valid_opaque_id(deletion_id) {
            return invalid(
                "campaign deletion preparation",
                deletion_id,
                "expected lifecycle revision and deletion id are invalid",
            );
        }
        let exported = self
            .export_campaign_private(owner_key, campaign_session_id)
            .await?;
        let canonical_export_json = exported.canonical_json()?;
        let canonical_export_digest = digest(canonical_export_json.as_bytes());
        let mut transaction = self.pool.begin().await.map_err(RepositoryError::Database)?;
        let locked = lock_owned_campaign(&mut transaction, owner_key, campaign_session_id).await?;
        locked.require_revision(expected_lifecycle_revision)?;
        locked.require_state(CampaignLifecycleState::Archived)?;
        require_no_open_play_session(&mut transaction, campaign_session_id).await?;
        if locked.campaign_revision != exported.campaign.document.revision
            || locked.lifecycle_revision != exported.campaign.lifecycle_revision
        {
            return invalid(
                "campaign deletion preparation",
                deletion_id,
                "campaign changed while its delete export was being prepared",
            );
        }
        sqlx::query(
            "INSERT INTO campaign_deletion_preparations
             (owner_key, campaign_session_id, deletion_id, campaign_revision,
              lifecycle_revision, canonical_export_digest, canonical_export_json)
             VALUES ($1, $2, $3, $4, $5, $6, $7)
             ON CONFLICT (owner_key, campaign_session_id, deletion_id) DO NOTHING",
        )
        .bind(owner_key)
        .bind(campaign_session_id)
        .bind(deletion_id)
        .bind(to_i64(locked.campaign_revision, "campaign revision")?)
        .bind(to_i64(locked.lifecycle_revision, "lifecycle revision")?)
        .bind(canonical_export_digest.as_str())
        .bind(&canonical_export_json)
        .execute(&mut *transaction)
        .await
        .map_err(RepositoryError::Database)?;
        let row = sqlx::query(
            "SELECT campaign_revision, lifecycle_revision, canonical_export_digest,
                    canonical_export_json, expires_at::text AS expires_at
             FROM campaign_deletion_preparations
             WHERE owner_key = $1 AND campaign_session_id = $2 AND deletion_id = $3",
        )
        .bind(owner_key)
        .bind(campaign_session_id)
        .bind(deletion_id)
        .fetch_one(&mut *transaction)
        .await
        .map_err(RepositoryError::Database)?;
        let stored_digest: String = row
            .try_get("canonical_export_digest")
            .map_err(RepositoryError::Database)?;
        let stored_json: String = row
            .try_get("canonical_export_json")
            .map_err(RepositoryError::Database)?;
        let stored_campaign_revision = from_i64(
            row.try_get("campaign_revision")
                .map_err(RepositoryError::Database)?,
            "prepared campaign revision",
        )?;
        let stored_lifecycle_revision = from_i64(
            row.try_get("lifecycle_revision")
                .map_err(RepositoryError::Database)?,
            "prepared lifecycle revision",
        )?;
        if stored_digest != canonical_export_digest.as_str()
            || stored_json != canonical_export_json
            || stored_campaign_revision != locked.campaign_revision
            || stored_lifecycle_revision != locked.lifecycle_revision
        {
            return invalid(
                "campaign deletion preparation",
                deletion_id,
                "deletion id was already used for another snapshot",
            );
        }
        let prepared = PreparedCampaignDeletion {
            schema_version: CAMPAIGN_LIFECYCLE_SCHEMA_VERSION,
            campaign_session_id: campaign_session_id.to_owned(),
            deletion_id: deletion_id.to_owned(),
            campaign_revision: stored_campaign_revision,
            lifecycle_revision: stored_lifecycle_revision,
            canonical_export_digest,
            canonical_export_json: stored_json,
            expires_at: row
                .try_get("expires_at")
                .map_err(RepositoryError::Database)?,
        };
        transaction
            .commit()
            .await
            .map_err(RepositoryError::Database)?;
        Ok(prepared)
    }
}
