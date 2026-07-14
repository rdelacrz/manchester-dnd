use std::{str::FromStr, time::Duration};

use manchester_dnd_core::{
    Character, EventActor, SESSION_SCHEMA_VERSION, SessionDto, SessionEventDto,
    SessionEventPayload, SessionStatus, Sha256Digest, is_valid_opaque_id,
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sqlx::{
    Row, SqlitePool,
    migrate::Migrator,
    sqlite::{SqliteConnectOptions, SqlitePoolOptions},
};

use crate::error::RepositoryError;

static MIGRATOR: Migrator = sqlx::migrate!("../../migrations");
pub const CHARACTER_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SaveOutcome {
    pub revision: u64,
    pub updated_at: String,
}

#[derive(Debug, Clone, Copy)]
pub struct CharacterUpdate<'a> {
    pub character: &'a Character,
    pub expected_revision: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CharacterCommitOutcome {
    pub character_id: String,
    pub save: SaveOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CampaignCreateOutcome {
    pub session: SaveOutcome,
    pub characters: Vec<CharacterCommitOutcome>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionEventCommitOutcome {
    pub session: SaveOutcome,
    pub characters: Vec<CharacterCommitOutcome>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StoredDocument<T> {
    pub id: String,
    pub schema_version: u32,
    pub revision: u64,
    pub value: T,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TurnAudit<T> {
    pub id: String,
    pub campaign_session_id: String,
    pub turn_number: u64,
    pub actor_id: Option<String>,
    pub schema_version: u32,
    pub payload: T,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct NewGeneratedAssetAudit {
    pub id: String,
    pub campaign_session_id: String,
    pub turn_id: Option<String>,
    pub asset_kind: String,
    pub provider: String,
    pub model: String,
    pub location: String,
    /// A caller-provided digest. Raw prompts are intentionally not persisted by
    /// this repository because event prompts may contain personal information.
    pub prompt_fingerprint: Option<Sha256Digest>,
    pub metadata: GeneratedAssetMetadata,
}

/// Allowlisted, non-sensitive media facts. This intentionally has no field for
/// raw prompts, provider credentials, or arbitrary JSON.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GeneratedAssetMetadata {
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub media_type: Option<String>,
    pub provider_request_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GeneratedAssetAudit {
    pub id: String,
    pub campaign_session_id: String,
    pub turn_id: Option<String>,
    pub asset_kind: String,
    pub provider: String,
    pub model: String,
    pub location: String,
    pub prompt_fingerprint: Option<Sha256Digest>,
    pub metadata: GeneratedAssetMetadata,
    pub created_at: String,
}

#[derive(Debug, Clone)]
pub struct SqliteRepository {
    pool: SqlitePool,
}

impl SqliteRepository {
    pub async fn connect(database_url: &str) -> Result<Self, RepositoryError> {
        let options = SqliteConnectOptions::from_str(database_url)
            .map_err(RepositoryError::InvalidDatabaseUrl)?
            .create_if_missing(true)
            .foreign_keys(true)
            .busy_timeout(Duration::from_secs(5));
        let max_connections = if database_url.contains(":memory:") {
            1
        } else {
            5
        };
        let pool = SqlitePoolOptions::new()
            .max_connections(max_connections)
            .connect_with(options)
            .await
            .map_err(RepositoryError::Database)?;
        let repository = Self { pool };
        repository.migrate().await?;
        Ok(repository)
    }

    pub async fn migrate(&self) -> Result<(), RepositoryError> {
        MIGRATOR
            .run(&self.pool)
            .await
            .map_err(RepositoryError::Migration)
    }

    /// Atomically creates an initial campaign and its complete declared party.
    /// Subsequent authoritative changes must go through `commit_session_event`.
    pub async fn create_campaign(
        &self,
        session: &SessionDto,
        characters: &[Character],
    ) -> Result<CampaignCreateOutcome, RepositoryError> {
        validate_session(session)?;
        if session.last_event_sequence != 0 || session.status != SessionStatus::Active {
            return Err(RepositoryError::InvalidDomainState {
                entity: "campaign session",
                id: session.id.clone(),
                reason: "a new session must be active and start before its first event",
            });
        }
        validate_initial_roster(session, characters)?;
        let session_payload = serialize("campaign session", session)?;
        let character_payloads = characters
            .iter()
            .map(|character| {
                serialize("character", character).map(|payload| (character.id(), payload))
            })
            .collect::<Result<Vec<_>, _>>()?;

        let mut transaction = self.pool.begin().await.map_err(RepositoryError::Database)?;
        let session_row = sqlx::query(
            "INSERT INTO campaign_sessions
             (id, schema_version, revision, payload_json)
             VALUES (?, ?, 1, ?)
             RETURNING updated_at",
        )
        .bind(&session.id)
        .bind(i64::from(SESSION_SCHEMA_VERSION))
        .bind(session_payload)
        .fetch_one(&mut *transaction)
        .await
        .map_err(|error| map_insert_error(error, "campaign session", &session.id))?;
        let session_save = SaveOutcome {
            revision: 1,
            updated_at: session_row
                .try_get("updated_at")
                .map_err(RepositoryError::Database)?,
        };

        let mut character_outcomes = Vec::with_capacity(character_payloads.len());
        for (character_id, payload) in character_payloads {
            let row = sqlx::query(
                "INSERT INTO characters
                 (id, campaign_session_id, schema_version, revision, payload_json)
                 VALUES (?, ?, ?, 1, ?)
                 RETURNING updated_at",
            )
            .bind(character_id)
            .bind(&session.id)
            .bind(i64::from(CHARACTER_SCHEMA_VERSION))
            .bind(payload)
            .fetch_one(&mut *transaction)
            .await
            .map_err(|error| map_insert_error(error, "character", character_id))?;
            character_outcomes.push(CharacterCommitOutcome {
                character_id: character_id.to_owned(),
                save: SaveOutcome {
                    revision: 1,
                    updated_at: row
                        .try_get("updated_at")
                        .map_err(RepositoryError::Database)?,
                },
            });
        }
        transaction
            .commit()
            .await
            .map_err(RepositoryError::Database)?;
        Ok(CampaignCreateOutcome {
            session: session_save,
            characters: character_outcomes,
        })
    }

    pub async fn load_campaign_session(
        &self,
        id: &str,
    ) -> Result<Option<StoredDocument<SessionDto>>, RepositoryError> {
        let stored = load_document(
            &self.pool,
            DocumentTable::CampaignSession,
            "SELECT id, schema_version, revision, payload_json, created_at, updated_at
             FROM campaign_sessions WHERE id = ?",
            id,
        )
        .await?;
        stored.map(validate_stored_session).transpose()
    }

    pub async fn load_character(
        &self,
        id: &str,
    ) -> Result<Option<StoredDocument<Character>>, RepositoryError> {
        let stored = load_document(
            &self.pool,
            DocumentTable::Character,
            "SELECT id, schema_version, revision, payload_json, created_at, updated_at
             FROM characters WHERE id = ?",
            id,
        )
        .await?;
        stored.map(validate_stored_character).transpose()
    }

    /// Commits a post-event session snapshot and its append-only audit row in
    /// one transaction. The optimistic revision and event sequence prevent a
    /// stale caller from skipping or duplicating a turn.
    pub async fn commit_session_event(
        &self,
        audit_id: &str,
        session: &SessionDto,
        expected_revision: u64,
        event: &SessionEventDto,
        character_updates: &[CharacterUpdate<'_>],
    ) -> Result<SessionEventCommitOutcome, RepositoryError> {
        validate_session(session)?;
        validate_session_event(event)?;
        if !is_valid_opaque_id(audit_id) {
            return Err(RepositoryError::InvalidDomainState {
                entity: "session event",
                id: format!("{}:{}", event.session_id, event.sequence),
                reason: "audit id must be a valid opaque identifier",
            });
        }
        if session.id != event.session_id || session.last_event_sequence != event.sequence {
            return Err(RepositoryError::InvalidDomainState {
                entity: "session event",
                id: audit_id.to_owned(),
                reason: "session snapshot and event identity or sequence do not match",
            });
        }
        if event_references_unknown_character(session, event) {
            return Err(RepositoryError::InvalidDomainState {
                entity: "session event",
                id: audit_id.to_owned(),
                reason: "event references a character outside the campaign session",
            });
        }
        validate_character_update_set(session, event, character_updates)?;

        let next_revision = expected_revision
            .checked_add(1)
            .ok_or(RepositoryError::NumericRange { field: "revision" })?;
        let session_payload = serialize("campaign session", session)?;
        let event_payload = serialize("session event", event)?;
        let actor_id = match &event.actor {
            EventActor::Player { character_id } => Some(character_id.clone()),
            EventActor::AiGameMaster | EventActor::System => None,
        };

        let mut transaction = self.pool.begin().await.map_err(RepositoryError::Database)?;
        let current_row = sqlx::query(
            "SELECT schema_version, revision, payload_json
             FROM campaign_sessions WHERE id = ?",
        )
        .bind(&session.id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(RepositoryError::Database)?
        .ok_or_else(|| RepositoryError::NotFound {
            entity: "campaign session",
            id: session.id.clone(),
        })?;

        let actual_revision = from_i64(
            current_row
                .try_get("revision")
                .map_err(RepositoryError::Database)?,
            "revision",
        )?;
        if actual_revision != expected_revision {
            return Err(RepositoryError::RevisionConflict {
                entity: "campaign session",
                id: session.id.clone(),
                expected: expected_revision,
                actual: actual_revision,
            });
        }

        let stored_schema_version = from_i64_u32(
            current_row
                .try_get("schema_version")
                .map_err(RepositoryError::Database)?,
            "schema_version",
        )?;
        if stored_schema_version != u32::from(SESSION_SCHEMA_VERSION) {
            return Err(RepositoryError::UnsupportedSchemaVersion {
                entity: "campaign session",
                found: stored_schema_version,
                supported: u32::from(SESSION_SCHEMA_VERSION),
            });
        }
        let current_payload: String = current_row
            .try_get("payload_json")
            .map_err(RepositoryError::Database)?;
        let current: SessionDto = serde_json::from_str(&current_payload).map_err(|source| {
            RepositoryError::InvalidStoredData {
                entity: "campaign session",
                id: session.id.clone(),
                source,
            }
        })?;
        if current.id != session.id {
            return Err(RepositoryError::IdentityMismatch {
                entity: "campaign session",
                row_id: session.id.clone(),
                payload_id: current.id,
            });
        }
        validate_session(&current)?;

        let expected_sequence =
            current
                .last_event_sequence
                .checked_add(1)
                .ok_or(RepositoryError::NumericRange {
                    field: "event sequence",
                })?;
        if event.sequence != expected_sequence {
            return Err(RepositoryError::InvalidDomainState {
                entity: "session event",
                id: audit_id.to_owned(),
                reason: "event sequence must immediately follow the stored session sequence",
            });
        }
        if session.ruleset != current.ruleset
            || session.created_at_unix_ms != current.created_at_unix_ms
        {
            return Err(RepositoryError::InvalidDomainState {
                entity: "campaign session",
                id: session.id.clone(),
                reason: "ruleset and creation timestamp are immutable",
            });
        }
        if session.title != current.title || session.character_ids != current.character_ids {
            return Err(RepositoryError::InvalidDomainState {
                entity: "campaign session",
                id: session.id.clone(),
                reason: "a turn event cannot rewrite session metadata or party membership",
            });
        }
        let valid_status_transition = match event.payload {
            SessionEventPayload::SessionEnded => {
                current.status == SessionStatus::Active
                    && session.status == SessionStatus::Completed
            }
            _ => current.status == SessionStatus::Active && session.status == current.status,
        };
        if !valid_status_transition {
            return Err(RepositoryError::InvalidDomainState {
                entity: "campaign session",
                id: session.id.clone(),
                reason: "session status transition is invalid for this event",
            });
        }
        if event.occurred_at_unix_ms < current.updated_at_unix_ms
            || session.updated_at_unix_ms < current.updated_at_unix_ms
            || session.updated_at_unix_ms < event.occurred_at_unix_ms
        {
            return Err(RepositoryError::InvalidDomainState {
                entity: "campaign session",
                id: session.id.clone(),
                reason: "event and session timestamps must advance monotonically",
            });
        }

        let mut committed_characters = Vec::with_capacity(character_updates.len());
        for update in character_updates {
            let character_id = update.character.id();
            let row = sqlx::query(
                "SELECT campaign_session_id, schema_version, revision, payload_json
                 FROM characters WHERE id = ?",
            )
            .bind(character_id)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(RepositoryError::Database)?
            .ok_or_else(|| RepositoryError::NotFound {
                entity: "character",
                id: character_id.to_owned(),
            })?;
            let linked_session_id: Option<String> = row
                .try_get("campaign_session_id")
                .map_err(RepositoryError::Database)?;
            if linked_session_id.as_deref() != Some(session.id.as_str()) {
                return Err(RepositoryError::InvalidDomainState {
                    entity: "character",
                    id: character_id.to_owned(),
                    reason: "character is not linked to this campaign session",
                });
            }
            let character_schema = from_i64_u32(
                row.try_get("schema_version")
                    .map_err(RepositoryError::Database)?,
                "schema_version",
            )?;
            if character_schema != CHARACTER_SCHEMA_VERSION {
                return Err(RepositoryError::UnsupportedSchemaVersion {
                    entity: "character",
                    found: character_schema,
                    supported: CHARACTER_SCHEMA_VERSION,
                });
            }
            let actual_character_revision = from_i64(
                row.try_get("revision").map_err(RepositoryError::Database)?,
                "revision",
            )?;
            if actual_character_revision != update.expected_revision {
                return Err(RepositoryError::RevisionConflict {
                    entity: "character",
                    id: character_id.to_owned(),
                    expected: update.expected_revision,
                    actual: actual_character_revision,
                });
            }
            let current_character_payload: String = row
                .try_get("payload_json")
                .map_err(RepositoryError::Database)?;
            let current_character: Character = serde_json::from_str(&current_character_payload)
                .map_err(|source| RepositoryError::InvalidStoredData {
                    entity: "character",
                    id: character_id.to_owned(),
                    source,
                })?;
            if current_character.id() != character_id {
                return Err(RepositoryError::IdentityMismatch {
                    entity: "character",
                    row_id: character_id.to_owned(),
                    payload_id: current_character.id().to_owned(),
                });
            }
            current_character
                .validate()
                .map_err(|source| RepositoryError::CoreValidation {
                    entity: "character",
                    id: character_id.to_owned(),
                    source,
                })?;

            if let SessionEventPayload::ExperienceAwarded {
                character_id: awarded_character_id,
                summary,
            } = &event.payload
            {
                let mut expected_character = current_character;
                let expected_summary = expected_character
                    .award_experience(summary.awarded)
                    .map_err(|source| RepositoryError::CoreValidation {
                        entity: "character",
                        id: character_id.to_owned(),
                        source,
                    })?;
                if awarded_character_id != character_id
                    || &expected_summary != summary
                    || &expected_character != update.character
                {
                    return Err(RepositoryError::InvalidDomainState {
                        entity: "character",
                        id: character_id.to_owned(),
                        reason: "character snapshot does not match the audited XP award",
                    });
                }
            }

            let next_character_revision = update
                .expected_revision
                .checked_add(1)
                .ok_or(RepositoryError::NumericRange { field: "revision" })?;
            let character_payload = serialize("character", update.character)?;
            let updated_character_row = sqlx::query(
                "UPDATE characters
                 SET schema_version = ?, revision = ?, payload_json = ?,
                     updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
                 WHERE id = ? AND campaign_session_id = ? AND revision = ?
                 RETURNING updated_at",
            )
            .bind(i64::from(CHARACTER_SCHEMA_VERSION))
            .bind(to_i64(next_character_revision, "revision")?)
            .bind(character_payload)
            .bind(character_id)
            .bind(&session.id)
            .bind(to_i64(update.expected_revision, "revision")?)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(RepositoryError::Database)?
            .ok_or_else(|| RepositoryError::RevisionConflict {
                entity: "character",
                id: character_id.to_owned(),
                expected: update.expected_revision,
                actual: actual_character_revision,
            })?;
            committed_characters.push(CharacterCommitOutcome {
                character_id: character_id.to_owned(),
                save: SaveOutcome {
                    revision: next_character_revision,
                    updated_at: updated_character_row
                        .try_get("updated_at")
                        .map_err(RepositoryError::Database)?,
                },
            });
        }

        let updated_row = sqlx::query(
            "UPDATE campaign_sessions
             SET schema_version = ?, revision = ?, payload_json = ?,
                 updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
             WHERE id = ? AND revision = ?
             RETURNING updated_at",
        )
        .bind(i64::from(SESSION_SCHEMA_VERSION))
        .bind(to_i64(next_revision, "revision")?)
        .bind(session_payload)
        .bind(&session.id)
        .bind(to_i64(expected_revision, "revision")?)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(RepositoryError::Database)?
        .ok_or_else(|| RepositoryError::RevisionConflict {
            entity: "campaign session",
            id: session.id.clone(),
            expected: expected_revision,
            actual: actual_revision,
        })?;
        let updated_at = updated_row
            .try_get("updated_at")
            .map_err(RepositoryError::Database)?;

        sqlx::query(
            "INSERT INTO turn_audits
             (id, campaign_session_id, turn_number, actor_id, schema_version, payload_json)
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(audit_id)
        .bind(&event.session_id)
        .bind(to_i64(event.sequence, "event sequence")?)
        .bind(actor_id)
        .bind(i64::from(SESSION_SCHEMA_VERSION))
        .bind(event_payload)
        .execute(&mut *transaction)
        .await
        .map_err(RepositoryError::Database)?;

        transaction
            .commit()
            .await
            .map_err(RepositoryError::Database)?;
        Ok(SessionEventCommitOutcome {
            session: SaveOutcome {
                revision: next_revision,
                updated_at,
            },
            characters: committed_characters,
        })
    }

    async fn list_turns<T>(
        &self,
        campaign_session_id: &str,
    ) -> Result<Vec<TurnAudit<T>>, RepositoryError>
    where
        T: DeserializeOwned,
    {
        let rows = sqlx::query(
            "SELECT id, campaign_session_id, turn_number, actor_id, schema_version,
                    payload_json, created_at
             FROM turn_audits
             WHERE campaign_session_id = ?
             ORDER BY turn_number",
        )
        .bind(campaign_session_id)
        .fetch_all(&self.pool)
        .await
        .map_err(RepositoryError::Database)?;

        rows.into_iter()
            .map(|row| {
                let id: String = row.try_get("id").map_err(RepositoryError::Database)?;
                let payload_json: String = row
                    .try_get("payload_json")
                    .map_err(RepositoryError::Database)?;
                let payload = serde_json::from_str(&payload_json).map_err(|source| {
                    RepositoryError::InvalidStoredData {
                        entity: "turn audit",
                        id: id.clone(),
                        source,
                    }
                })?;
                Ok(TurnAudit {
                    id,
                    campaign_session_id: row
                        .try_get("campaign_session_id")
                        .map_err(RepositoryError::Database)?,
                    turn_number: from_i64(
                        row.try_get("turn_number")
                            .map_err(RepositoryError::Database)?,
                        "turn_number",
                    )?,
                    actor_id: row.try_get("actor_id").map_err(RepositoryError::Database)?,
                    schema_version: from_i64_u32(
                        row.try_get("schema_version")
                            .map_err(RepositoryError::Database)?,
                        "schema_version",
                    )?,
                    payload,
                    created_at: row
                        .try_get("created_at")
                        .map_err(RepositoryError::Database)?,
                })
            })
            .collect()
    }

    pub async fn list_session_events(
        &self,
        campaign_session_id: &str,
    ) -> Result<Vec<TurnAudit<SessionEventDto>>, RepositoryError> {
        let events = self.list_turns(campaign_session_id).await?;
        for event in &events {
            if event.schema_version != u32::from(SESSION_SCHEMA_VERSION) {
                return Err(RepositoryError::UnsupportedSchemaVersion {
                    entity: "session event",
                    found: event.schema_version,
                    supported: u32::from(SESSION_SCHEMA_VERSION),
                });
            }
            validate_session_event(&event.payload)?;
            if event.campaign_session_id != event.payload.session_id
                || event.turn_number != event.payload.sequence
            {
                return Err(RepositoryError::InvalidDomainState {
                    entity: "session event",
                    id: event.id.clone(),
                    reason: "audit columns do not match the payload",
                });
            }
        }
        Ok(events)
    }

    pub async fn record_generated_asset(
        &self,
        asset: &NewGeneratedAssetAudit,
    ) -> Result<(), RepositoryError> {
        validate_generated_asset(asset)?;
        let metadata = serialize("generated asset metadata", &asset.metadata)?;
        sqlx::query(
            "INSERT INTO generated_assets
             (id, campaign_session_id, turn_id, asset_kind, provider, model, location,
              prompt_fingerprint, metadata_json)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&asset.id)
        .bind(&asset.campaign_session_id)
        .bind(&asset.turn_id)
        .bind(&asset.asset_kind)
        .bind(&asset.provider)
        .bind(&asset.model)
        .bind(&asset.location)
        .bind(asset.prompt_fingerprint.as_ref().map(Sha256Digest::as_str))
        .bind(metadata)
        .execute(&self.pool)
        .await
        .map_err(RepositoryError::Database)?;
        Ok(())
    }

    pub async fn list_generated_assets(
        &self,
        campaign_session_id: &str,
    ) -> Result<Vec<GeneratedAssetAudit>, RepositoryError> {
        let rows = sqlx::query(
            "SELECT id, campaign_session_id, turn_id, asset_kind, provider, model, location,
                    prompt_fingerprint, metadata_json, created_at
             FROM generated_assets
             WHERE campaign_session_id = ?
             ORDER BY created_at, id",
        )
        .bind(campaign_session_id)
        .fetch_all(&self.pool)
        .await
        .map_err(RepositoryError::Database)?;

        rows.into_iter()
            .map(|row| {
                let id: String = row.try_get("id").map_err(RepositoryError::Database)?;
                let metadata_json: String = row
                    .try_get("metadata_json")
                    .map_err(RepositoryError::Database)?;
                let metadata = serde_json::from_str(&metadata_json).map_err(|source| {
                    RepositoryError::InvalidStoredData {
                        entity: "generated asset metadata",
                        id: id.clone(),
                        source,
                    }
                })?;
                let raw_fingerprint: Option<String> = row
                    .try_get("prompt_fingerprint")
                    .map_err(RepositoryError::Database)?;
                let prompt_fingerprint = raw_fingerprint
                    .map(Sha256Digest::new)
                    .transpose()
                    .map_err(|source| RepositoryError::CoreValidation {
                        entity: "generated asset",
                        id: id.clone(),
                        source,
                    })?;
                let asset = GeneratedAssetAudit {
                    id,
                    campaign_session_id: row
                        .try_get("campaign_session_id")
                        .map_err(RepositoryError::Database)?,
                    turn_id: row.try_get("turn_id").map_err(RepositoryError::Database)?,
                    asset_kind: row
                        .try_get("asset_kind")
                        .map_err(RepositoryError::Database)?,
                    provider: row.try_get("provider").map_err(RepositoryError::Database)?,
                    model: row.try_get("model").map_err(RepositoryError::Database)?,
                    location: row.try_get("location").map_err(RepositoryError::Database)?,
                    prompt_fingerprint,
                    metadata,
                    created_at: row
                        .try_get("created_at")
                        .map_err(RepositoryError::Database)?,
                };
                validate_generated_asset_fields(
                    &asset.id,
                    &asset.campaign_session_id,
                    asset.turn_id.as_deref(),
                    &asset.asset_kind,
                    &asset.provider,
                    &asset.model,
                    &asset.location,
                    &asset.metadata,
                )?;
                Ok(asset)
            })
            .collect()
    }
}

#[derive(Clone, Copy)]
enum DocumentTable {
    CampaignSession,
    Character,
}

impl DocumentTable {
    fn entity(self) -> &'static str {
        match self {
            Self::CampaignSession => "campaign session",
            Self::Character => "character",
        }
    }
}

async fn load_document<T>(
    pool: &SqlitePool,
    table: DocumentTable,
    query: &'static str,
    id: &str,
) -> Result<Option<StoredDocument<T>>, RepositoryError>
where
    T: DeserializeOwned,
{
    let row = sqlx::query(query)
        .bind(id)
        .fetch_optional(pool)
        .await
        .map_err(RepositoryError::Database)?;
    row.map(|row| {
        let stored_id: String = row.try_get("id").map_err(RepositoryError::Database)?;
        let payload_json: String = row
            .try_get("payload_json")
            .map_err(RepositoryError::Database)?;
        let value = serde_json::from_str(&payload_json).map_err(|source| {
            RepositoryError::InvalidStoredData {
                entity: table.entity(),
                id: stored_id.clone(),
                source,
            }
        })?;
        Ok(StoredDocument {
            id: stored_id,
            schema_version: from_i64_u32(
                row.try_get("schema_version")
                    .map_err(RepositoryError::Database)?,
                "schema_version",
            )?,
            revision: from_i64(
                row.try_get("revision").map_err(RepositoryError::Database)?,
                "revision",
            )?,
            value,
            created_at: row
                .try_get("created_at")
                .map_err(RepositoryError::Database)?,
            updated_at: row
                .try_get("updated_at")
                .map_err(RepositoryError::Database)?,
        })
    })
    .transpose()
}

fn serialize<T>(entity: &'static str, value: &T) -> Result<String, RepositoryError>
where
    T: Serialize,
{
    serde_json::to_string(value).map_err(|source| RepositoryError::Serialize { entity, source })
}

fn to_i64(value: u64, field: &'static str) -> Result<i64, RepositoryError> {
    i64::try_from(value).map_err(|_| RepositoryError::NumericRange { field })
}

fn from_i64(value: i64, field: &'static str) -> Result<u64, RepositoryError> {
    u64::try_from(value).map_err(|_| RepositoryError::NumericRange { field })
}

fn from_i64_u32(value: i64, field: &'static str) -> Result<u32, RepositoryError> {
    u32::try_from(value).map_err(|_| RepositoryError::NumericRange { field })
}

fn validate_session(session: &SessionDto) -> Result<(), RepositoryError> {
    if session.schema_version != SESSION_SCHEMA_VERSION {
        return Err(RepositoryError::UnsupportedSchemaVersion {
            entity: "campaign session",
            found: u32::from(session.schema_version),
            supported: u32::from(SESSION_SCHEMA_VERSION),
        });
    }
    session
        .validate()
        .map_err(|source| RepositoryError::CoreValidation {
            entity: "campaign session",
            id: session.id.clone(),
            source,
        })
}

fn validate_initial_roster(
    session: &SessionDto,
    characters: &[Character],
) -> Result<(), RepositoryError> {
    let mut supplied_ids = std::collections::BTreeSet::new();
    for character in characters {
        character
            .validate()
            .map_err(|source| RepositoryError::CoreValidation {
                entity: "character",
                id: character.id().to_owned(),
                source,
            })?;
        if !supplied_ids.insert(character.id()) {
            return Err(RepositoryError::InvalidDomainState {
                entity: "campaign session",
                id: session.id.clone(),
                reason: "initial character snapshots must have unique ids",
            });
        }
    }
    let declared_ids = session
        .character_ids
        .iter()
        .map(String::as_str)
        .collect::<std::collections::BTreeSet<_>>();
    if declared_ids != supplied_ids {
        return Err(RepositoryError::InvalidDomainState {
            entity: "campaign session",
            id: session.id.clone(),
            reason: "declared party ids must exactly match initial character snapshots",
        });
    }
    Ok(())
}

fn map_insert_error(error: sqlx::Error, entity: &'static str, id: &str) -> RepositoryError {
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

fn validate_stored_session(
    stored: StoredDocument<SessionDto>,
) -> Result<StoredDocument<SessionDto>, RepositoryError> {
    if stored.schema_version != u32::from(SESSION_SCHEMA_VERSION) {
        return Err(RepositoryError::UnsupportedSchemaVersion {
            entity: "campaign session",
            found: stored.schema_version,
            supported: u32::from(SESSION_SCHEMA_VERSION),
        });
    }
    if stored.id != stored.value.id {
        return Err(RepositoryError::IdentityMismatch {
            entity: "campaign session",
            row_id: stored.id,
            payload_id: stored.value.id,
        });
    }
    validate_session(&stored.value)?;
    Ok(stored)
}

fn validate_stored_character(
    stored: StoredDocument<Character>,
) -> Result<StoredDocument<Character>, RepositoryError> {
    if stored.schema_version != CHARACTER_SCHEMA_VERSION {
        return Err(RepositoryError::UnsupportedSchemaVersion {
            entity: "character",
            found: stored.schema_version,
            supported: CHARACTER_SCHEMA_VERSION,
        });
    }
    if stored.id != stored.value.id() {
        return Err(RepositoryError::IdentityMismatch {
            entity: "character",
            row_id: stored.id,
            payload_id: stored.value.id().to_owned(),
        });
    }
    stored
        .value
        .validate()
        .map_err(|source| RepositoryError::CoreValidation {
            entity: "character",
            id: stored.id.clone(),
            source,
        })?;
    Ok(stored)
}

fn validate_session_event(event: &SessionEventDto) -> Result<(), RepositoryError> {
    if event.schema_version != SESSION_SCHEMA_VERSION {
        return Err(RepositoryError::UnsupportedSchemaVersion {
            entity: "session event",
            found: u32::from(event.schema_version),
            supported: u32::from(SESSION_SCHEMA_VERSION),
        });
    }
    event
        .validate()
        .map_err(|source| RepositoryError::CoreValidation {
            entity: "session event",
            id: format!("{}:{}", event.session_id, event.sequence),
            source,
        })
}

fn event_references_unknown_character(session: &SessionDto, event: &SessionEventDto) -> bool {
    let known = |id: &str| session.character_ids.iter().any(|known| known == id);
    match &event.actor {
        EventActor::Player { character_id } if !known(character_id) => return true,
        EventActor::Player { .. } | EventActor::AiGameMaster | EventActor::System => {}
    }
    match &event.payload {
        SessionEventPayload::PlayerIntent { character_id, .. }
        | SessionEventPayload::ExperienceAwarded { character_id, .. } => !known(character_id),
        _ => false,
    }
}

fn validate_character_update_set(
    session: &SessionDto,
    event: &SessionEventDto,
    updates: &[CharacterUpdate<'_>],
) -> Result<(), RepositoryError> {
    let mut ids = std::collections::BTreeSet::new();
    for update in updates {
        update
            .character
            .validate()
            .map_err(|source| RepositoryError::CoreValidation {
                entity: "character",
                id: update.character.id().to_owned(),
                source,
            })?;
        if !ids.insert(update.character.id())
            || !session
                .character_ids
                .iter()
                .any(|id| id == update.character.id())
        {
            return Err(RepositoryError::InvalidDomainState {
                entity: "session event",
                id: format!("{}:{}", event.session_id, event.sequence),
                reason: "character updates must be unique members of the campaign",
            });
        }
    }

    match &event.payload {
        SessionEventPayload::ExperienceAwarded { character_id, .. }
            if updates.len() == 1 && updates[0].character.id() == character_id =>
        {
            Ok(())
        }
        SessionEventPayload::ExperienceAwarded { .. } => Err(RepositoryError::InvalidDomainState {
            entity: "session event",
            id: format!("{}:{}", event.session_id, event.sequence),
            reason: "an XP event requires exactly one matching character update",
        }),
        _ if updates.is_empty() => Ok(()),
        _ => Err(RepositoryError::InvalidDomainState {
            entity: "session event",
            id: format!("{}:{}", event.session_id, event.sequence),
            reason: "this event type cannot mutate a character",
        }),
    }
}

fn validate_generated_asset(asset: &NewGeneratedAssetAudit) -> Result<(), RepositoryError> {
    validate_generated_asset_fields(
        &asset.id,
        &asset.campaign_session_id,
        asset.turn_id.as_deref(),
        &asset.asset_kind,
        &asset.provider,
        &asset.model,
        &asset.location,
        &asset.metadata,
    )
}

#[allow(clippy::too_many_arguments)]
fn validate_generated_asset_fields(
    id: &str,
    campaign_session_id: &str,
    turn_id: Option<&str>,
    asset_kind: &str,
    provider: &str,
    model: &str,
    location: &str,
    metadata: &GeneratedAssetMetadata,
) -> Result<(), RepositoryError> {
    let bounded =
        |value: &str, maximum: usize| !value.trim().is_empty() && value.chars().count() <= maximum;
    if !is_valid_opaque_id(id)
        || !is_valid_opaque_id(campaign_session_id)
        || turn_id.is_some_and(|id| !is_valid_opaque_id(id))
        || !is_valid_opaque_id(asset_kind)
        || !is_valid_opaque_id(provider)
        || !bounded(model, 256)
        || !valid_asset_key(location)
        || metadata
            .media_type
            .as_ref()
            .is_some_and(|media_type| !valid_media_type(media_type))
        || metadata
            .provider_request_id
            .as_ref()
            .is_some_and(|id| !is_valid_opaque_id(id))
        || !matches!(
            (metadata.width, metadata.height),
            (None, None) | (Some(1..=32_768), Some(1..=32_768))
        )
    {
        return Err(RepositoryError::InvalidDomainState {
            entity: "generated asset",
            id: id.to_owned(),
            reason: "identifiers, location, fingerprint, and media metadata must be bounded",
        });
    }
    Ok(())
}

fn valid_asset_key(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 2_048
        && !value.starts_with('/')
        && value.split('/').all(|segment| {
            !segment.is_empty()
                && !matches!(segment, "." | "..")
                && segment
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        })
}

fn valid_media_type(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.contains('/')
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || matches!(byte, b'/' | b'+' | b'-' | b'.')
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use manchester_dnd_core::{
        AbilityScores, CharacterDraft, RULESET, SESSION_SCHEMA_VERSION, SessionEventPayload,
        SessionStatus,
    };

    fn session() -> SessionDto {
        SessionDto {
            schema_version: SESSION_SCHEMA_VERSION,
            id: "session-1".to_owned(),
            ruleset: RULESET,
            title: "Rain over Ancoats".to_owned(),
            status: SessionStatus::Active,
            character_ids: vec!["character-1".to_owned()],
            created_at_unix_ms: 1,
            updated_at_unix_ms: 2,
            last_event_sequence: 0,
        }
    }

    fn character() -> Character {
        character_with_id("character-1")
    }

    fn character_with_id(id: &str) -> Character {
        CharacterDraft {
            id: id.to_owned(),
            name: "Mancunian Wizard".to_owned(),
            theme: "rainbound occultist".to_owned(),
            ability_scores: AbilityScores::new(12, 14, 10, 16, 13, 8).expect("valid scores"),
            experience_points: 0,
            current_hit_points: 8,
            maximum_hit_points: 8,
        }
        .build()
        .expect("valid character")
    }

    async fn repository() -> SqliteRepository {
        SqliteRepository::connect("sqlite::memory:")
            .await
            .expect("in-memory repository should initialize")
    }

    #[tokio::test]
    async fn creates_loads_and_rejects_duplicate_sessions() {
        let repository = repository().await;
        let initial = session();
        let party = [character()];

        let created = repository
            .create_campaign(&initial, &party)
            .await
            .expect("create should succeed");
        assert_eq!(created.session.revision, 1);
        assert_eq!(created.characters.len(), 1);

        let duplicate = repository
            .create_campaign(&initial, &party)
            .await
            .expect_err("duplicate session should fail");
        assert!(matches!(duplicate, RepositoryError::AlreadyExists { .. }));

        let loaded = repository
            .load_campaign_session("session-1")
            .await
            .expect("load should succeed")
            .expect("document should exist");
        assert_eq!(loaded.value, initial);
        assert_eq!(loaded.revision, 1);
    }

    #[tokio::test]
    async fn campaign_creation_rolls_back_if_any_character_conflicts() {
        let repository = repository().await;
        repository
            .create_campaign(&session(), &[character()])
            .await
            .expect("first campaign should save");

        let mut second = session();
        second.id = "session-2".to_owned();
        second.title = "The Other Rain".to_owned();
        second.character_ids = vec!["character-2".to_owned(), "character-1".to_owned()];
        let party = [character_with_id("character-2"), character()];
        repository
            .create_campaign(&second, &party)
            .await
            .expect_err("a colliding character must roll back the whole campaign");

        assert!(
            repository
                .load_campaign_session("session-2")
                .await
                .expect("session lookup should succeed")
                .is_none()
        );
        assert!(
            repository
                .load_character("character-2")
                .await
                .expect("character lookup should succeed")
                .is_none()
        );
    }

    #[tokio::test]
    async fn new_campaign_must_start_active() {
        let repository = repository().await;
        let mut completed = session();
        completed.status = SessionStatus::Completed;

        repository
            .create_campaign(&completed, &[character()])
            .await
            .expect_err("a completed campaign cannot be created as new");
        assert!(
            repository
                .load_campaign_session("session-1")
                .await
                .expect("session lookup should succeed")
                .is_none()
        );
    }

    #[tokio::test]
    async fn event_timestamps_cannot_move_backwards() {
        let repository = repository().await;
        let initial = session();
        repository
            .create_campaign(&initial, &[character()])
            .await
            .expect("campaign should save");
        let event = SessionEventDto {
            schema_version: SESSION_SCHEMA_VERSION,
            session_id: initial.id.clone(),
            sequence: 1,
            occurred_at_unix_ms: 1,
            actor: EventActor::System,
            payload: SessionEventPayload::SessionStarted,
        };
        let mut submitted = initial;
        submitted.last_event_sequence = 1;
        submitted.updated_at_unix_ms = 3;

        repository
            .commit_session_event("turn-1", &submitted, 1, &event, &[])
            .await
            .expect_err("event time cannot predate the stored session update");
        let stored = repository
            .load_campaign_session("session-1")
            .await
            .expect("session should load")
            .expect("session should exist");
        assert_eq!(stored.value.last_event_sequence, 0);
        assert_eq!(stored.revision, 1);
    }

    #[tokio::test]
    async fn persists_character_turn_and_generated_asset_audits() {
        let repository = repository().await;
        let initial = session();
        let mut advanced_character = character();
        repository
            .create_campaign(&initial, &[advanced_character.clone()])
            .await
            .expect("campaign and party should save");
        let award = advanced_character
            .award_experience(300)
            .expect("XP award should resolve");
        let event = SessionEventDto {
            schema_version: SESSION_SCHEMA_VERSION,
            session_id: "session-1".to_owned(),
            sequence: 1,
            occurred_at_unix_ms: 5,
            actor: EventActor::System,
            payload: SessionEventPayload::ExperienceAwarded {
                character_id: "character-1".to_owned(),
                summary: award,
            },
        };
        let mut next = initial;
        next.last_event_sequence = 1;
        next.updated_at_unix_ms = 5;
        let committed = repository
            .commit_session_event(
                "turn-1",
                &next,
                1,
                &event,
                &[CharacterUpdate {
                    character: &advanced_character,
                    expected_revision: 1,
                }],
            )
            .await
            .expect("session, character, and turn audit should save atomically");
        assert_eq!(committed.session.revision, 2);
        assert_eq!(committed.characters[0].save.revision, 2);
        repository
            .record_generated_asset(&NewGeneratedAssetAudit {
                id: "asset-1".to_owned(),
                campaign_session_id: "session-1".to_owned(),
                turn_id: Some("turn-1".to_owned()),
                asset_kind: "scene-image".to_owned(),
                provider: "openai-compatible".to_owned(),
                model: "illustrator".to_owned(),
                location: "assets/scene-1.webp".to_owned(),
                prompt_fingerprint: Some(
                    Sha256Digest::new(format!("sha256:{}", "a".repeat(64)))
                        .expect("valid test digest"),
                ),
                metadata: GeneratedAssetMetadata {
                    width: Some(1024),
                    height: Some(1024),
                    media_type: Some("image/webp".to_owned()),
                    provider_request_id: None,
                },
            })
            .await
            .expect("asset audit should save");

        let turns = repository
            .list_session_events("session-1")
            .await
            .expect("turns should load");
        let assets = repository
            .list_generated_assets("session-1")
            .await
            .expect("assets should load");
        assert_eq!(turns.len(), 1);
        assert!(matches!(
            turns[0].payload.payload,
            SessionEventPayload::ExperienceAwarded { .. }
        ));
        assert_eq!(assets.len(), 1);
        assert_eq!(assets[0].metadata.width, Some(1024));
        let loaded = repository
            .load_campaign_session("session-1")
            .await
            .expect("session should load")
            .expect("session should exist");
        assert_eq!(loaded.value.last_event_sequence, 1);
        let loaded_character = repository
            .load_character("character-1")
            .await
            .expect("character should load")
            .expect("character should exist");
        assert_eq!(loaded_character.value.experience_points(), 300);
        assert_eq!(loaded_character.revision, 2);
    }

    #[tokio::test]
    async fn mismatched_xp_commit_leaves_every_document_unchanged() {
        let repository = repository().await;
        let initial_session = session();
        let initial_character = character();
        repository
            .create_campaign(&initial_session, std::slice::from_ref(&initial_character))
            .await
            .expect("campaign and party should save");

        let mut submitted_character = initial_character.clone();
        submitted_character
            .award_experience(300)
            .expect("submitted update should be valid by itself");
        let mut differently_awarded = initial_character;
        let mismatched_summary = differently_awarded
            .award_experience(900)
            .expect("event summary should be valid by itself");
        let event = SessionEventDto {
            schema_version: SESSION_SCHEMA_VERSION,
            session_id: "session-1".to_owned(),
            sequence: 1,
            occurred_at_unix_ms: 5,
            actor: EventActor::System,
            payload: SessionEventPayload::ExperienceAwarded {
                character_id: "character-1".to_owned(),
                summary: mismatched_summary,
            },
        };
        let mut submitted_session = initial_session;
        submitted_session.last_event_sequence = 1;
        submitted_session.updated_at_unix_ms = 5;

        repository
            .commit_session_event(
                "turn-1",
                &submitted_session,
                1,
                &event,
                &[CharacterUpdate {
                    character: &submitted_character,
                    expected_revision: 1,
                }],
            )
            .await
            .expect_err("event and character XP must describe the same transition");

        let stored_session = repository
            .load_campaign_session("session-1")
            .await
            .expect("session should load")
            .expect("session should exist");
        let stored_character = repository
            .load_character("character-1")
            .await
            .expect("character should load")
            .expect("character should exist");
        assert_eq!(stored_session.revision, 1);
        assert_eq!(stored_session.value.last_event_sequence, 0);
        assert_eq!(stored_character.revision, 1);
        assert_eq!(stored_character.value.experience_points(), 0);
        assert!(
            repository
                .list_session_events("session-1")
                .await
                .expect("event list should load")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn generated_asset_cannot_reference_another_campaigns_turn() {
        let repository = repository().await;
        let first = session();
        repository
            .create_campaign(&first, &[character()])
            .await
            .expect("first session should save");
        let event = SessionEventDto {
            schema_version: SESSION_SCHEMA_VERSION,
            session_id: first.id.clone(),
            sequence: 1,
            occurred_at_unix_ms: 5,
            actor: EventActor::System,
            payload: SessionEventPayload::SessionStarted,
        };
        let mut first_after = first;
        first_after.last_event_sequence = 1;
        first_after.updated_at_unix_ms = 5;
        repository
            .commit_session_event("turn-1", &first_after, 1, &event, &[])
            .await
            .expect("first turn should commit");

        let mut second = session();
        second.id = "session-2".to_owned();
        second.title = "Another campaign".to_owned();
        second.character_ids.clear();
        repository
            .create_campaign(&second, &[])
            .await
            .expect("second session should save");

        let error = repository
            .record_generated_asset(&NewGeneratedAssetAudit {
                id: "cross-campaign-asset".to_owned(),
                campaign_session_id: second.id,
                turn_id: Some("turn-1".to_owned()),
                asset_kind: "scene-image".to_owned(),
                provider: "openai-compatible".to_owned(),
                model: "illustrator".to_owned(),
                location: "assets/cross-campaign.webp".to_owned(),
                prompt_fingerprint: None,
                metadata: GeneratedAssetMetadata::default(),
            })
            .await
            .expect_err("a turn from another campaign must be rejected");
        assert!(matches!(error, RepositoryError::Database(_)));
    }

    #[tokio::test]
    async fn generated_assets_are_revalidated_when_loaded() {
        let repository = repository().await;
        repository
            .create_campaign(&session(), &[character()])
            .await
            .expect("campaign should save");
        sqlx::query(
            "INSERT INTO generated_assets
             (id, campaign_session_id, asset_kind, provider, model, location, metadata_json)
             VALUES ('asset-unsafe', 'session-1', 'scene-image', 'test-provider',
                     'model', '../../secret', '{}')",
        )
        .execute(&repository.pool)
        .await
        .expect("fixture bypasses the repository write boundary");

        let error = repository
            .list_generated_assets("session-1")
            .await
            .expect_err("unsafe legacy/imported rows must not cross the read boundary");
        assert!(matches!(
            error,
            RepositoryError::InvalidDomainState {
                entity: "generated asset",
                ..
            }
        ));
    }
}
