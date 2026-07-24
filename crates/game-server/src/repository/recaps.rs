//! MongoDB owner-private recaps derived only from committed turn events.

#![allow(dead_code)]

use manchester_dnd_core::{SessionEventDto, SessionEventPayload, Sha256Digest, is_valid_opaque_id};
use mongodb::{
    ClientSession, Collection,
    bson::{DateTime, Document, doc},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::{CampaignDocument, MongoRepository, lifecycle::TurnEventDocument};
use crate::{
    error::{MongoFailureKind, PersistenceError, RepositoryError},
    persistence::CollectionName,
};

pub const PRIVATE_RECAP_SCHEMA_VERSION: u16 = 1;
const PRIVATE_RECAP_TEMPLATE_ID: &str = "private-recap-v1";
const MAX_PRIVATE_RECAP_BYTES: usize = 128 * 1024;
const MAX_RECAP_SOURCE_EVENTS: usize = 4_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GeneratePrivateRecapCommand {
    pub schema_version: u16,
    pub campaign_session_id: String,
    pub expected_campaign_revision: u64,
    pub idempotency_key: String,
}

impl GeneratePrivateRecapCommand {
    pub fn validate(&self) -> Result<(), RepositoryError> {
        if self.schema_version != PRIVATE_RECAP_SCHEMA_VERSION {
            return Err(RepositoryError::UnsupportedSchemaVersion {
                entity: "private recap command",
                found: u32::from(self.schema_version),
                supported: u32::from(PRIVATE_RECAP_SCHEMA_VERSION),
            });
        }
        if !is_valid_opaque_id(&self.campaign_session_id)
            || !is_valid_opaque_id(&self.idempotency_key)
            || self.expected_campaign_revision == 0
        {
            return invalid(
                "private recap command",
                &self.campaign_session_id,
                "identity or expected revision is invalid",
            );
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CampaignPrivateRecap {
    pub schema_version: u16,
    pub id: String,
    pub campaign_session_id: String,
    pub campaign_revision: u64,
    pub idempotency_key: String,
    pub request_fingerprint: Sha256Digest,
    pub first_turn_number: Option<u64>,
    pub last_turn_number: Option<u64>,
    pub source_audit_count: u64,
    pub source_audit_digest: Sha256Digest,
    pub template_id: String,
    pub body: String,
    pub body_digest: Sha256Digest,
    pub created_at: String,
}

impl CampaignPrivateRecap {
    pub(crate) fn validate_for_campaign(
        &self,
        campaign_session_id: &str,
        maximum_campaign_revision: u64,
    ) -> Result<(), RepositoryError> {
        let valid_turn_range = match self.source_audit_count {
            0 => self.first_turn_number.is_none() && self.last_turn_number.is_none(),
            _ => self
                .first_turn_number
                .zip(self.last_turn_number)
                .is_some_and(|(first, last)| first > 0 && last >= first),
        };
        if self.schema_version != PRIVATE_RECAP_SCHEMA_VERSION
            || self.campaign_session_id != campaign_session_id
            || !is_valid_opaque_id(&self.id)
            || !is_valid_opaque_id(&self.idempotency_key)
            || self.campaign_revision == 0
            || self.campaign_revision > maximum_campaign_revision
            || !valid_turn_range
            || self.template_id != PRIVATE_RECAP_TEMPLATE_ID
            || self.body.is_empty()
            || self.body.len() > MAX_PRIVATE_RECAP_BYTES
            || digest(self.body.as_bytes()) != self.body_digest
        {
            return invalid(
                "private campaign recap",
                &self.id,
                "identity, source range, provenance, or body integrity is invalid",
            );
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RecapPresentationDocument {
    #[serde(rename = "_id")]
    id: String,
    schema_version: i64,
    campaign_id: String,
    owner_account_id: String,
    origin_event_id: String,
    version: i64,
    selected: bool,
    presentation_type: String,
    campaign_revision: i64,
    idempotency_key: String,
    request_fingerprint: Sha256Digest,
    first_turn_number: Option<i64>,
    last_turn_number: Option<i64>,
    source_audit_count: i64,
    source_audit_digest: Sha256Digest,
    template_id: String,
    body: String,
    body_digest: Sha256Digest,
    created_at: DateTime,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RecapReceiptDocument {
    #[serde(rename = "_id")]
    id: String,
    schema_version: i64,
    scope_kind: String,
    scope_id: String,
    campaign_id: String,
    actor_account_id: String,
    command_kind: String,
    idempotency_key: String,
    request_fingerprint: Sha256Digest,
    expected_revision: i64,
    result_revision: i64,
    response: Document,
    state: String,
    retain_after_delete: bool,
    created_at: DateTime,
    purge_at: Option<DateTime>,
}

#[derive(Serialize)]
struct RecapAuditDigestInput<'a> {
    id: &'a str,
    turn_number: u64,
    event: &'a SessionEventDto,
}

struct RecapSourceTurn {
    id: String,
    turn_number: u64,
    event: SessionEventDto,
}

impl MongoRepository {
    pub async fn generate_private_recap(
        &self,
        owner_key: &str,
        command: &GeneratePrivateRecapCommand,
    ) -> Result<CampaignPrivateRecap, RepositoryError> {
        validate_owner(owner_key)?;
        command.validate()?;
        let request_fingerprint = command_fingerprint(command)?;
        let campaigns = self.campaigns();
        let presentations = self
            .store()
            .collection::<RecapPresentationDocument>(CollectionName::GeneratedPresentations);
        let turns = self
            .store()
            .collection::<TurnEventDocument>(CollectionName::TurnEvents);
        let receipts = self
            .store()
            .collection::<RecapReceiptDocument>(CollectionName::CommandReceipts);
        let owner = owner_key.to_owned();
        let command_owned = command.clone();
        let campaign_id = command.campaign_session_id.clone();
        self.with_transaction(move |session| {
            let campaigns = campaigns.clone();
            let presentations = presentations.clone();
            let turns = turns.clone();
            let receipts = receipts.clone();
            let owner = owner.clone();
            let command = command_owned.clone();
            let request_fingerprint = request_fingerprint.clone();
            Box::pin(async move {
                if let Some(replay) =
                    load_recap_replay(&receipts, session, &owner, &command, &request_fingerprint)
                        .await?
                {
                    return Ok(replay);
                }
                let campaign =
                    load_owned_campaign(&campaigns, session, &owner, &command.campaign_session_id)
                        .await?;
                let current_revision = nonnegative_u64(campaign.revision);
                if current_revision != command.expected_campaign_revision {
                    return Err(PersistenceError::RevisionConflict {
                        entity: "campaign_session",
                        id: campaign.id,
                        expected: command.expected_campaign_revision,
                        actual: current_revision,
                    });
                }
                let origin_event_id =
                    format!("campaign-revision:{}", command.expected_campaign_revision);
                if let Some(existing) = presentations
                    .find_one(doc! {
                        "campaign_id": &command.campaign_session_id,
                        "owner_account_id": &owner,
                        "origin_event_id": &origin_event_id,
                        "presentation_type": "private_recap",
                        "selected": true,
                    })
                    .session(&mut *session)
                    .await
                    .map_err(|error| {
                        PersistenceError::mongo("load recap for campaign revision", error)
                    })?
                {
                    let recap = recap_from_document(existing).map_err(repository_to_persistence)?;
                    insert_recap_receipt(
                        &receipts,
                        session,
                        &owner,
                        &command,
                        request_fingerprint,
                        &recap,
                    )
                    .await?;
                    return Ok(recap);
                }
                let source_turns =
                    load_source_turns(&turns, session, &command.campaign_session_id).await?;
                let source_audit_digest =
                    audit_digest(&source_turns).map_err(repository_to_persistence)?;
                let body = render_recap(
                    &campaign.title,
                    current_revision,
                    &source_audit_digest,
                    &source_turns,
                )
                .map_err(repository_to_persistence)?;
                let body_digest = digest(body.as_bytes());
                let id = format!("private-recap:{}", Uuid::new_v4());
                let source_count = i64::try_from(source_turns.len()).map_err(|_| {
                    PersistenceError::SchemaDrift {
                        collection: "turn_events".to_owned(),
                        detail: "recap source event count is outside the supported range"
                            .to_owned(),
                    }
                })?;
                let document = RecapPresentationDocument {
                    id,
                    schema_version: i64::from(PRIVATE_RECAP_SCHEMA_VERSION),
                    campaign_id: command.campaign_session_id.clone(),
                    owner_account_id: owner.clone(),
                    origin_event_id,
                    version: 1,
                    selected: true,
                    presentation_type: "private_recap".to_owned(),
                    campaign_revision: to_i64_persistence(current_revision)?,
                    idempotency_key: command.idempotency_key.clone(),
                    request_fingerprint: request_fingerprint.clone(),
                    first_turn_number: source_turns
                        .first()
                        .map(|turn| to_i64_persistence(turn.turn_number))
                        .transpose()?,
                    last_turn_number: source_turns
                        .last()
                        .map(|turn| to_i64_persistence(turn.turn_number))
                        .transpose()?,
                    source_audit_count: source_count,
                    source_audit_digest,
                    template_id: PRIVATE_RECAP_TEMPLATE_ID.to_owned(),
                    body,
                    body_digest,
                    created_at: DateTime::now(),
                };
                presentations
                    .insert_one(document.clone())
                    .session(&mut *session)
                    .await
                    .map_err(|error| {
                        PersistenceError::mongo("store private recap presentation", error)
                    })?;
                let recap = recap_from_document(document).map_err(repository_to_persistence)?;
                insert_recap_receipt(
                    &receipts,
                    session,
                    &owner,
                    &command,
                    request_fingerprint,
                    &recap,
                )
                .await?;
                Ok(recap)
            })
        })
        .await
        .map_err(|error| map_transaction_error(error, "private campaign recap", &campaign_id))
    }

    pub async fn load_latest_private_recap(
        &self,
        owner_key: &str,
        campaign_session_id: &str,
    ) -> Result<Option<CampaignPrivateRecap>, RepositoryError> {
        validate_owner(owner_key)?;
        if !is_valid_opaque_id(campaign_session_id) {
            return invalid(
                "campaign session",
                campaign_session_id,
                "campaign identity is invalid",
            );
        }
        let campaign = self
            .campaigns()
            .find_one(doc! {
                "_id": campaign_session_id,
                "owner_account_id": owner_key,
                "members": {
                    "$elemMatch": {
                        "account_id": owner_key,
                        "role": "game_master",
                        "state": "active",
                    }
                },
            })
            .await
            .map_err(|error| mongo_error("authorize private recap read", error))?
            .ok_or_else(|| RepositoryError::NotFound {
                entity: "campaign_session",
                id: campaign_session_id.to_owned(),
            })?;
        self.store()
            .collection::<RecapPresentationDocument>(CollectionName::GeneratedPresentations)
            .find_one(doc! {
                "campaign_id": campaign_session_id,
                "owner_account_id": owner_key,
                "presentation_type": "private_recap",
                "selected": true,
            })
            .sort(doc! { "campaign_revision": -1, "created_at": -1, "_id": -1 })
            .await
            .map_err(|error| mongo_error("load latest private recap", error))?
            .map(recap_from_document)
            .transpose()?
            .map(|recap| {
                recap.validate_for_campaign(
                    campaign_session_id,
                    nonnegative_u64(campaign.revision),
                )?;
                Ok(recap)
            })
            .transpose()
    }
}

async fn load_owned_campaign(
    campaigns: &Collection<CampaignDocument>,
    session: &mut ClientSession,
    owner_key: &str,
    campaign_id: &str,
) -> Result<CampaignDocument, PersistenceError> {
    campaigns
        .find_one(doc! {
            "_id": campaign_id,
            "owner_account_id": owner_key,
            "members": {
                "$elemMatch": {
                    "account_id": owner_key,
                    "role": "game_master",
                    "state": "active",
                }
            },
        })
        .session(&mut *session)
        .await
        .map_err(|error| PersistenceError::mongo("authorize private recap", error))?
        .ok_or_else(|| PersistenceError::NotFound {
            entity: "campaign_session",
            id: campaign_id.to_owned(),
        })
}

async fn load_source_turns(
    turns: &Collection<TurnEventDocument>,
    session: &mut ClientSession,
    campaign_id: &str,
) -> Result<Vec<RecapSourceTurn>, PersistenceError> {
    let mut cursor = turns
        .find(doc! { "campaign_id": campaign_id })
        .sort(doc! { "sequence": 1, "_id": 1 })
        .limit(
            i64::try_from(MAX_RECAP_SOURCE_EVENTS.saturating_add(1)).map_err(|_| {
                PersistenceError::SchemaDrift {
                    collection: "turn_events".to_owned(),
                    detail: "recap source limit is outside the supported range".to_owned(),
                }
            })?,
        )
        .session(&mut *session)
        .await
        .map_err(|error| PersistenceError::mongo("load private recap source events", error))?;
    let mut documents = Vec::new();
    while let Some(document) = cursor
        .next(&mut *session)
        .await
        .transpose()
        .map_err(|error| PersistenceError::mongo("read private recap source events", error))?
    {
        documents.push(document);
    }
    if documents.len() > MAX_RECAP_SOURCE_EVENTS {
        return Err(PersistenceError::SchemaDrift {
            collection: "turn_events".to_owned(),
            detail: "recap source event count exceeds the supported bound".to_owned(),
        });
    }
    documents
        .into_iter()
        .map(|document| {
            if document.sequence <= 0
                || document.event.session_id != campaign_id
                || document.event.sequence != nonnegative_u64(document.sequence)
            {
                return Err(PersistenceError::SchemaDrift {
                    collection: "turn_events".to_owned(),
                    detail: "recap source event envelope is inconsistent".to_owned(),
                });
            }
            document
                .event
                .validate()
                .map_err(|_| PersistenceError::SchemaDrift {
                    collection: "turn_events".to_owned(),
                    detail: "recap source event failed domain validation".to_owned(),
                })?;
            Ok(RecapSourceTurn {
                id: document.id,
                turn_number: nonnegative_u64(document.sequence),
                event: document.event,
            })
        })
        .collect()
}

async fn load_recap_replay(
    receipts: &Collection<RecapReceiptDocument>,
    session: &mut ClientSession,
    owner_key: &str,
    command: &GeneratePrivateRecapCommand,
    fingerprint: &Sha256Digest,
) -> Result<Option<CampaignPrivateRecap>, PersistenceError> {
    let receipt = receipts
        .find_one(doc! {
            "scope_kind": "campaign",
            "scope_id": &command.campaign_session_id,
            "actor_account_id": owner_key,
            "idempotency_key": &command.idempotency_key,
            "state": "committed",
        })
        .session(&mut *session)
        .await
        .map_err(|error| PersistenceError::mongo("load private recap receipt", error))?;
    let Some(receipt) = receipt else {
        return Ok(None);
    };
    if receipt.command_kind != "private_recap_generate"
        || receipt.request_fingerprint != *fingerprint
    {
        return Err(PersistenceError::AlreadyExists {
            entity: "private recap idempotency key",
            id: command.idempotency_key.clone(),
        });
    }
    mongodb::bson::from_document(receipt.response)
        .map(Some)
        .map_err(|_| PersistenceError::SchemaDrift {
            collection: "command_receipts".to_owned(),
            detail: "private recap receipt response is invalid".to_owned(),
        })
}

async fn insert_recap_receipt(
    receipts: &Collection<RecapReceiptDocument>,
    session: &mut ClientSession,
    owner_key: &str,
    command: &GeneratePrivateRecapCommand,
    fingerprint: Sha256Digest,
    recap: &CampaignPrivateRecap,
) -> Result<(), PersistenceError> {
    receipts
        .insert_one(RecapReceiptDocument {
            id: format!("command-receipt:{}", Uuid::new_v4()),
            schema_version: 1,
            scope_kind: "campaign".to_owned(),
            scope_id: command.campaign_session_id.clone(),
            campaign_id: command.campaign_session_id.clone(),
            actor_account_id: owner_key.to_owned(),
            command_kind: "private_recap_generate".to_owned(),
            idempotency_key: command.idempotency_key.clone(),
            request_fingerprint: fingerprint,
            expected_revision: to_i64_persistence(command.expected_campaign_revision)?,
            result_revision: to_i64_persistence(recap.campaign_revision)?,
            response: mongodb::bson::to_document(recap).map_err(PersistenceError::BsonEncoding)?,
            state: "committed".to_owned(),
            retain_after_delete: false,
            created_at: DateTime::now(),
            purge_at: None,
        })
        .session(&mut *session)
        .await
        .map_err(|error| PersistenceError::mongo("store private recap receipt", error))?;
    Ok(())
}

fn recap_from_document(
    document: RecapPresentationDocument,
) -> Result<CampaignPrivateRecap, RepositoryError> {
    if document.presentation_type != "private_recap" || !document.selected || document.version != 1
    {
        return invalid(
            "private campaign recap",
            &document.id,
            "presentation type, selection, or version is invalid",
        );
    }
    let recap = CampaignPrivateRecap {
        schema_version: u16::try_from(document.schema_version).map_err(|_| {
            RepositoryError::NumericRange {
                field: "private recap schema version",
            }
        })?,
        id: document.id,
        campaign_session_id: document.campaign_id,
        campaign_revision: nonnegative_u64(document.campaign_revision),
        idempotency_key: document.idempotency_key,
        request_fingerprint: document.request_fingerprint,
        first_turn_number: document.first_turn_number.map(nonnegative_u64),
        last_turn_number: document.last_turn_number.map(nonnegative_u64),
        source_audit_count: nonnegative_u64(document.source_audit_count),
        source_audit_digest: document.source_audit_digest,
        template_id: document.template_id,
        body: document.body,
        body_digest: document.body_digest,
        created_at: date_string(document.created_at)?,
    };
    recap.validate_for_campaign(&recap.campaign_session_id, recap.campaign_revision)?;
    Ok(recap)
}

fn audit_digest(turns: &[RecapSourceTurn]) -> Result<Sha256Digest, RepositoryError> {
    let input = turns
        .iter()
        .map(|turn| RecapAuditDigestInput {
            id: &turn.id,
            turn_number: turn.turn_number,
            event: &turn.event,
        })
        .collect::<Vec<_>>();
    let encoded = serde_json::to_vec(&input).map_err(|source| RepositoryError::Serialize {
        entity: "private recap audit digest input",
        source,
    })?;
    Ok(digest(&encoded))
}

fn render_recap(
    title: &str,
    campaign_revision: u64,
    source_audit_digest: &Sha256Digest,
    turns: &[RecapSourceTurn],
) -> Result<String, RepositoryError> {
    let mut body = format!(
        "# Private campaign recap\n\nCampaign: {}\n\nCampaign revision: {campaign_revision}\n\nProvenance: {PRIVATE_RECAP_TEMPLATE_ID}; {} committed event{}; source {}.\n\n",
        escape_markdown_line(title),
        turns.len(),
        if turns.len() == 1 { "" } else { "s" },
        source_audit_digest,
    );
    if turns.is_empty() {
        body.push_str("No committed turns have been recorded yet.\n");
    } else {
        body.push_str("## Saved events\n\n");
        for turn in turns {
            body.push_str(&format!(
                "- Turn {} — {}\n",
                turn.turn_number,
                recap_line(&turn.event.payload)
            ));
        }
    }
    if body.len() > MAX_PRIVATE_RECAP_BYTES {
        return invalid(
            "private campaign recap",
            "rendered-body",
            "recap exceeds its durable body limit",
        );
    }
    Ok(body)
}

fn recap_line(payload: &SessionEventPayload) -> String {
    match payload {
        SessionEventPayload::SessionStarted => "The campaign began.".to_owned(),
        SessionEventPayload::PlayerIntent { .. } => {
            "A player intent was committed; free-form wording is omitted from the recap.".to_owned()
        }
        SessionEventPayload::DiceResolved {
            purpose,
            total,
            modifier,
            ..
        } => format!(
            "The {} roll resolved to {total} with modifier {modifier}.",
            escape_markdown_line(purpose)
        ),
        SessionEventPayload::AbilityCheckResolved {
            action_id, result, ..
        } => format!(
            "The authored {} check {} with total {} against DC {}.",
            escape_markdown_line(action_id),
            if result.success { "succeeded" } else { "failed" },
            result.total,
            result.difficulty_class,
        ),
        SessionEventPayload::ExplorationSocialResolved { outcome, .. } => format!(
            "The authored lockkeeper conversation {} with total {} against DC {}; its objective, clock, and attitude changes were saved.",
            if outcome.check.result.outcome
                == manchester_dnd_core::rules_matrix::D20TestOutcome::Success
            {
                "succeeded"
            } else {
                "failed"
            },
            outcome.check.result.total,
            outcome.check.difficulty.difficulty_class,
        ),
        SessionEventPayload::EncounterResolved { outcome, .. } => {
            escape_markdown_line(&outcome.resolution.narration.authored_text)
        }
        SessionEventPayload::GmNarration {
            text,
            source_prompt_id,
            ..
        } if source_prompt_id.is_none() => escape_markdown_line(text),
        SessionEventPayload::GmNarration { .. } => {
            "A private-inspired presentation was committed; its wording is omitted because recap consent is independently scoped.".to_owned()
        }
        SessionEventPayload::ExperienceAwarded { summary, .. } => format!(
            "A trusted reward added {} XP, bringing the saved total to {} XP.",
            summary.awarded, summary.experience_points
        ),
        SessionEventPayload::AiProposalAccepted { .. } => {
            "A typed model proposal passed deterministic validation.".to_owned()
        }
        SessionEventPayload::AiProposalRejected { reason, .. } => format!(
            "A typed model proposal was rejected: {}.",
            escape_markdown_line(reason)
        ),
        SessionEventPayload::SessionEnded => "The campaign session ended.".to_owned(),
    }
}

fn escape_markdown_line(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.trim().chars().take(4_000) {
        match character {
            '\n' | '\r' | '\t' => escaped.push(' '),
            '\\' | '`' | '*' | '_' | '{' | '}' | '[' | ']' | '<' | '>' | '(' | ')' | '#' | '+'
            | '-' | '.' | '!' | '|' => {
                escaped.push('\\');
                escaped.push(character);
            }
            character if character.is_control() => escaped.push(' '),
            character => escaped.push(character),
        }
    }
    escaped
}

fn command_fingerprint(
    command: &GeneratePrivateRecapCommand,
) -> Result<Sha256Digest, RepositoryError> {
    let encoded = serde_json::to_vec(command).map_err(|source| RepositoryError::Serialize {
        entity: "private recap command",
        source,
    })?;
    Ok(digest(&encoded))
}

fn digest(bytes: &[u8]) -> Sha256Digest {
    Sha256Digest::from_bytes(Sha256::digest(bytes).into())
}

fn validate_owner(owner_key: &str) -> Result<(), RepositoryError> {
    if is_valid_opaque_id(owner_key) {
        Ok(())
    } else {
        invalid("campaign owner", owner_key, "owner key is invalid")
    }
}

fn date_string(value: DateTime) -> Result<String, RepositoryError> {
    value.try_to_rfc3339_string().map_err(|_| {
        RepositoryError::Persistence(PersistenceError::SchemaDrift {
            collection: "generated_presentations".to_owned(),
            detail: "stored BSON date is outside RFC 3339 range".to_owned(),
        })
    })
}

fn nonnegative_u64(value: i64) -> u64 {
    if value < 0 { 0 } else { value as u64 }
}

fn to_i64_persistence(value: u64) -> Result<i64, PersistenceError> {
    i64::try_from(value).map_err(|_| PersistenceError::SchemaDrift {
        collection: "generated_presentations".to_owned(),
        detail: "recap revision is outside the supported range".to_owned(),
    })
}

fn repository_to_persistence(error: RepositoryError) -> PersistenceError {
    match error {
        RepositoryError::NotFound { entity, id } => PersistenceError::NotFound { entity, id },
        RepositoryError::AlreadyExists { entity, id } => {
            PersistenceError::AlreadyExists { entity, id }
        }
        RepositoryError::RevisionConflict {
            entity,
            id,
            expected,
            actual,
        } => PersistenceError::RevisionConflict {
            entity,
            id,
            expected,
            actual,
        },
        _ => PersistenceError::SchemaDrift {
            collection: "generated_presentations".to_owned(),
            detail: "private recap failed validation".to_owned(),
        },
    }
}

fn mongo_error(operation: &'static str, error: mongodb::error::Error) -> RepositoryError {
    RepositoryError::Persistence(PersistenceError::mongo(operation, error))
}

fn map_transaction_error(
    error: PersistenceError,
    entity: &'static str,
    id: &str,
) -> RepositoryError {
    match error {
        PersistenceError::NotFound { entity, id } => RepositoryError::NotFound { entity, id },
        PersistenceError::AlreadyExists { entity, id } => {
            RepositoryError::AlreadyExists { entity, id }
        }
        PersistenceError::RevisionConflict {
            entity,
            id,
            expected,
            actual,
        } => RepositoryError::RevisionConflict {
            entity,
            id,
            expected,
            actual,
        },
        other if other.mongo_failure_kind() == Some(MongoFailureKind::DuplicateKey) => {
            RepositoryError::AlreadyExists {
                entity,
                id: id.to_owned(),
            }
        }
        other if other.mongo_failure_kind() == Some(MongoFailureKind::DocumentValidation) => {
            RepositoryError::InvalidDomainState {
                entity,
                id: id.to_owned(),
                reason: "document failed MongoDB schema validation",
            }
        }
        other => RepositoryError::Persistence(other),
    }
}

fn invalid<T>(entity: &'static str, id: &str, reason: &'static str) -> Result<T, RepositoryError> {
    Err(RepositoryError::InvalidDomainState {
        entity,
        id: id.to_owned(),
        reason,
    })
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::{
        config::{MongoConfig, MongoSchemaPolicy, SecretString},
        persistence::{MongoStore, SchemaReconciler},
        repository::{
            CAMPAIGN_LIFECYCLE_SCHEMA_VERSION, CampaignLifecycleCommand, DeleteCampaignCommand,
        },
    };
    use manchester_dnd_core::{
        EventActor, SESSION_SCHEMA_VERSION, SessionEventDto, SessionEventPayload,
    };

    async fn test_repository() -> Option<(MongoRepository, String)> {
        let uri = std::env::var("MONGODB_TEST_URI").ok()?;
        if uri.trim().is_empty() {
            return None;
        }
        let database = format!("mdnd_test_recaps_{}", Uuid::new_v4().simple());
        let config = MongoConfig {
            uri: SecretString::new(uri),
            database: database.clone(),
            max_pool_size: 5,
            min_pool_size: 0,
            connect_timeout: Duration::from_secs(5),
            server_selection_timeout: Duration::from_secs(5),
            operation_timeout: Duration::from_secs(5),
            transaction_timeout: Duration::from_secs(10),
            transaction_max_retries: 3,
            schema_policy: MongoSchemaPolicy::ApplyAndVerify,
        };
        let store = MongoStore::connect(&config)
            .await
            .expect("test MongoDB must connect");
        SchemaReconciler::new(store.clone())
            .apply()
            .await
            .expect("schema must apply");
        Some((MongoRepository::new(store), database))
    }

    async fn insert_account(repository: &MongoRepository, account_id: &str) {
        repository
            .store()
            .document_collection(CollectionName::Accounts)
            .insert_one(doc! {
                "_id": account_id,
                "schema_version": 1_i64,
                "revision": 1_i64,
                "role": "user",
                "username_normalized": format!("user-{}", Uuid::new_v4()),
                "email_lookup_hmac": format!("hmac-sha256:{}", Uuid::new_v4().simple()),
                "password_phc": "$argon2id$test",
                "login_enabled": false,
                "created_at": DateTime::now(),
                "updated_at": DateTime::now(),
            })
            .await
            .expect("account fixture must insert");
    }

    #[tokio::test]
    async fn mongo_recap_is_turn_derived_owner_scoped_replayed_and_cascaded() {
        let Some((repository, database)) = test_repository().await else {
            return;
        };
        let owner = format!("account:{}", Uuid::new_v4());
        let outsider = format!("account:{}", Uuid::new_v4());
        insert_account(&repository, &owner).await;
        insert_account(&repository, &outsider).await;
        let campaign = repository
            .create_campaign_with_owner(
                &owner,
                "Rain *and* Brass",
                "dev.manchester-arcana.rainbound-borough",
            )
            .await
            .expect("campaign must create");
        let event = SessionEventDto {
            schema_version: SESSION_SCHEMA_VERSION,
            session_id: campaign.campaign_id.clone(),
            sequence: 1,
            occurred_at_unix_ms: 1,
            actor: EventActor::System,
            payload: SessionEventPayload::SessionStarted,
        };
        repository
            .store()
            .document_collection(CollectionName::TurnEvents)
            .insert_one(doc! {
                "_id": format!("turn-event:{}", Uuid::new_v4()),
                "schema_version": 1_i64,
                "campaign_id": &campaign.campaign_id,
                "play_session_id": format!("play-session:{}", Uuid::new_v4()),
                "sequence": 1_i64,
                "correlation_id": format!("correlation:{}", Uuid::new_v4()),
                "actor_account_id": mongodb::bson::Bson::Null,
                "event": mongodb::bson::to_bson(&event)
                    .expect("event fixture must encode"),
                "created_at": DateTime::now(),
            })
            .await
            .expect("turn fixture must insert");
        let command = GeneratePrivateRecapCommand {
            schema_version: PRIVATE_RECAP_SCHEMA_VERSION,
            campaign_session_id: campaign.campaign_id.clone(),
            expected_campaign_revision: 1,
            idempotency_key: "command:recap-once".to_owned(),
        };
        let recap = repository
            .generate_private_recap(&owner, &command)
            .await
            .expect("recap must generate");
        assert!(recap.body.contains("The campaign began."));
        assert_eq!(
            repository
                .generate_private_recap(&owner, &command)
                .await
                .expect("recap replay must work"),
            recap
        );
        assert!(matches!(
            repository
                .load_latest_private_recap(&outsider, &campaign.campaign_id)
                .await,
            Err(RepositoryError::NotFound { .. })
        ));
        repository
            .archive_campaign(
                &owner,
                &CampaignLifecycleCommand {
                    schema_version: CAMPAIGN_LIFECYCLE_SCHEMA_VERSION,
                    campaign_session_id: campaign.campaign_id.clone(),
                    expected_lifecycle_revision: 1,
                    idempotency_key: "command:archive-recap".to_owned(),
                },
            )
            .await
            .expect("archive must work");
        let prepared = repository
            .prepare_campaign_deletion(&owner, &campaign.campaign_id, 2, "deletion:recap-cascade")
            .await
            .expect("delete preparation must work");
        repository
            .delete_archived_campaign(
                &owner,
                &DeleteCampaignCommand {
                    lifecycle: CampaignLifecycleCommand {
                        schema_version: CAMPAIGN_LIFECYCLE_SCHEMA_VERSION,
                        campaign_session_id: campaign.campaign_id.clone(),
                        expected_lifecycle_revision: 2,
                        idempotency_key: "command:delete-recap".to_owned(),
                    },
                    deletion_id: prepared.deletion_id,
                    confirm_permanent_delete: true,
                },
            )
            .await
            .expect("campaign delete must work");
        assert!(
            repository
                .store()
                .document_collection(CollectionName::GeneratedPresentations)
                .find_one(doc! { "campaign_id": &campaign.campaign_id })
                .await
                .expect("presentation cascade read must work")
                .is_none()
        );

        assert!(
            database.starts_with("mdnd_test_recaps_"),
            "cleanup safeguard"
        );
        repository
            .store()
            .database()
            .drop()
            .await
            .expect("test database must drop");
    }
}
