//! Version-gated, one-time import for the retired SQLite storage schema.
//!
//! SQLite is opened immutable/read-only. Supported rows are validated through
//! the current domain and published in one PostgreSQL transaction. Exact replay
//! is idempotent; any target difference rolls the whole transaction back.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::Read,
    path::Path,
};

use manchester_dnd_core::{
    Character, SESSION_SCHEMA_VERSION, SessionDto, SessionEventDto, Sha256Digest,
    is_valid_opaque_id,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::{
    Connection, Row,
    sqlite::{SqliteConnectOptions, SqliteConnection, SqliteRow},
};
use thiserror::Error;

use super::{CHARACTER_SCHEMA_VERSION, PostgresRepository};

pub const LEGACY_IMPORT_SCHEMA_VERSION: u16 = 1;
const MAX_LEGACY_DATABASE_BYTES: u64 = 1024 * 1024 * 1024;
const MAX_LEGACY_CAMPAIGNS: usize = 100;
const MAX_LEGACY_ROWS: usize = 100_000;

#[derive(Debug, Error)]
pub enum LegacyImportError {
    #[error("legacy database storage failed")]
    Io(#[source] std::io::Error),
    #[error("legacy SQLite query failed")]
    Sqlite(#[source] sqlx::Error),
    #[error("legacy PostgreSQL import failed")]
    Postgres(#[source] sqlx::Error),
    #[error("legacy row JSON is invalid")]
    Json(#[source] serde_json::Error),
    #[error("legacy import validation failed: {0}")]
    Invalid(&'static str),
    #[error("legacy import target contains conflicting state")]
    TargetConflict,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LegacyImportCounts {
    pub campaign_sessions: u64,
    pub characters: u64,
    pub turn_audits: u64,
    pub command_receipts: u64,
    pub generated_assets: u64,
}

impl LegacyImportCounts {
    fn checked_sub(&self, other: &Self) -> Result<Self, LegacyImportError> {
        Ok(Self {
            campaign_sessions: self
                .campaign_sessions
                .checked_sub(other.campaign_sessions)
                .ok_or(LegacyImportError::TargetConflict)?,
            characters: self
                .characters
                .checked_sub(other.characters)
                .ok_or(LegacyImportError::TargetConflict)?,
            turn_audits: self
                .turn_audits
                .checked_sub(other.turn_audits)
                .ok_or(LegacyImportError::TargetConflict)?,
            command_receipts: self
                .command_receipts
                .checked_sub(other.command_receipts)
                .ok_or(LegacyImportError::TargetConflict)?,
            generated_assets: self
                .generated_assets
                .checked_sub(other.generated_assets)
                .ok_or(LegacyImportError::TargetConflict)?,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LegacyImportReport {
    pub schema_version: u16,
    pub source_database_digest: Sha256Digest,
    pub source_migration_versions: Vec<i64>,
    pub source_counts: LegacyImportCounts,
    pub inserted_counts: LegacyImportCounts,
    pub already_present_counts: LegacyImportCounts,
    pub source_state_digest: Sha256Digest,
    pub target_state_digest: Sha256Digest,
    pub timestamp_match_count: u64,
    pub committed: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct CampaignState {
    id: String,
    schema_version: u32,
    revision: u64,
    payload: Value,
}

#[derive(Debug, Clone)]
struct CampaignRow {
    state: CampaignState,
    created_at: String,
    updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct CharacterState {
    id: String,
    campaign_session_id: Option<String>,
    schema_version: u32,
    revision: u64,
    payload: Value,
}

#[derive(Debug, Clone)]
struct CharacterRow {
    state: CharacterState,
    created_at: String,
    updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct TurnState {
    id: String,
    campaign_session_id: String,
    turn_number: u64,
    actor_id: Option<String>,
    schema_version: u32,
    payload: Value,
}

#[derive(Debug, Clone)]
struct TurnRow {
    state: TurnState,
    created_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct ReceiptState {
    campaign_session_id: String,
    idempotency_key: String,
    command_kind: String,
    request_fingerprint: String,
    expected_revision: u64,
    result_revision: u64,
    audit_id: String,
    response: Value,
}

#[derive(Debug, Clone)]
struct ReceiptRow {
    state: ReceiptState,
    created_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct AssetState {
    id: String,
    campaign_session_id: String,
    turn_id: Option<String>,
    asset_kind: String,
    provider: String,
    model: String,
    location: String,
    prompt_fingerprint: Option<String>,
    metadata: Value,
}

#[derive(Debug, Clone)]
struct AssetRow {
    state: AssetState,
    created_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct LegacyState {
    campaigns: Vec<CampaignState>,
    characters: Vec<CharacterState>,
    turns: Vec<TurnState>,
    receipts: Vec<ReceiptState>,
    assets: Vec<AssetState>,
}

#[derive(Debug)]
struct LegacySnapshot {
    source_database_digest: Sha256Digest,
    migration_versions: Vec<i64>,
    campaigns: Vec<CampaignRow>,
    characters: Vec<CharacterRow>,
    turns: Vec<TurnRow>,
    receipts: Vec<ReceiptRow>,
    assets: Vec<AssetRow>,
}

impl LegacySnapshot {
    fn counts(&self) -> Result<LegacyImportCounts, LegacyImportError> {
        Ok(LegacyImportCounts {
            campaign_sessions: usize_to_u64(self.campaigns.len())?,
            characters: usize_to_u64(self.characters.len())?,
            turn_audits: usize_to_u64(self.turns.len())?,
            command_receipts: usize_to_u64(self.receipts.len())?,
            generated_assets: usize_to_u64(self.assets.len())?,
        })
    }

    fn state(&self) -> LegacyState {
        LegacyState {
            campaigns: self.campaigns.iter().map(|row| row.state.clone()).collect(),
            characters: self
                .characters
                .iter()
                .map(|row| row.state.clone())
                .collect(),
            turns: self.turns.iter().map(|row| row.state.clone()).collect(),
            receipts: self.receipts.iter().map(|row| row.state.clone()).collect(),
            assets: self.assets.iter().map(|row| row.state.clone()).collect(),
        }
    }
}

pub async fn import_legacy_sqlite(
    repository: &PostgresRepository,
    source_path: &Path,
    expected_source_digest: &Sha256Digest,
) -> Result<LegacyImportReport, LegacyImportError> {
    let snapshot = load_snapshot(source_path).await?;
    if &snapshot.source_database_digest != expected_source_digest {
        return Err(LegacyImportError::Invalid("source backup digest mismatch"));
    }
    validate_snapshot(&snapshot)?;
    let source_counts = snapshot.counts()?;
    let source_state = snapshot.state();
    let source_state_digest = state_digest(&source_state)?;
    let mut transaction = repository
        .pool
        .begin()
        .await
        .map_err(LegacyImportError::Postgres)?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL SERIALIZABLE")
        .execute(&mut *transaction)
        .await
        .map_err(LegacyImportError::Postgres)?;

    let mut inserted = LegacyImportCounts::default();
    for row in &snapshot.campaigns {
        inserted.campaign_sessions += sqlx::query(
            "INSERT INTO campaign_sessions
             (id, schema_version, revision, payload_json, created_at, updated_at,
              owner_key, lifecycle_revision, lifecycle_state, safety_policy_id,
              progression_policy_id, retention_class, content_pin_legacy_eligible)
             VALUES ($1, $2, $3, $4::jsonb, $5::timestamptz, $6::timestamptz,
                     'local-owner', 1, 'active', 'safety:private-mvp:v1',
                     'progression:srd-5.1-mvp:v1', 'campaign_lifetime', TRUE)
             ON CONFLICT (id) DO NOTHING",
        )
        .bind(&row.state.id)
        .bind(i64::from(row.state.schema_version))
        .bind(to_i64(row.state.revision)?)
        .bind(to_json(&row.state.payload)?)
        .bind(&row.created_at)
        .bind(&row.updated_at)
        .execute(&mut *transaction)
        .await
        .map_err(LegacyImportError::Postgres)?
        .rows_affected();
    }
    for row in &snapshot.characters {
        inserted.characters += sqlx::query(
            "INSERT INTO characters
             (id, campaign_session_id, schema_version, revision, payload_json,
              created_at, updated_at)
             VALUES ($1, $2, $3, $4, $5::jsonb, $6::timestamptz, $7::timestamptz)
             ON CONFLICT (id) DO NOTHING",
        )
        .bind(&row.state.id)
        .bind(row.state.campaign_session_id.as_deref())
        .bind(i64::from(row.state.schema_version))
        .bind(to_i64(row.state.revision)?)
        .bind(to_json(&row.state.payload)?)
        .bind(&row.created_at)
        .bind(&row.updated_at)
        .execute(&mut *transaction)
        .await
        .map_err(LegacyImportError::Postgres)?
        .rows_affected();
    }
    for row in &snapshot.turns {
        inserted.turn_audits += sqlx::query(
            "INSERT INTO turn_audits
             (id, campaign_session_id, turn_number, actor_id, correlation_id,
              schema_version, payload_json, created_at)
             VALUES ($1, $2, $3, $4, NULL, $5, $6::jsonb, $7::timestamptz)
             ON CONFLICT (id) DO NOTHING",
        )
        .bind(&row.state.id)
        .bind(&row.state.campaign_session_id)
        .bind(to_i64(row.state.turn_number)?)
        .bind(row.state.actor_id.as_deref())
        .bind(i64::from(row.state.schema_version))
        .bind(to_json(&row.state.payload)?)
        .bind(&row.created_at)
        .execute(&mut *transaction)
        .await
        .map_err(LegacyImportError::Postgres)?
        .rows_affected();
    }
    for row in &snapshot.receipts {
        inserted.command_receipts += sqlx::query(
            "INSERT INTO command_receipts
             (campaign_session_id, idempotency_key, command_kind,
              request_fingerprint, expected_revision, result_revision,
              audit_id, response_json, created_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9::timestamptz)
             ON CONFLICT (campaign_session_id, idempotency_key) DO NOTHING",
        )
        .bind(&row.state.campaign_session_id)
        .bind(&row.state.idempotency_key)
        .bind(&row.state.command_kind)
        .bind(&row.state.request_fingerprint)
        .bind(to_i64(row.state.expected_revision)?)
        .bind(to_i64(row.state.result_revision)?)
        .bind(&row.state.audit_id)
        .bind(to_json(&row.state.response)?)
        .bind(&row.created_at)
        .execute(&mut *transaction)
        .await
        .map_err(LegacyImportError::Postgres)?
        .rows_affected();
    }
    for row in &snapshot.assets {
        inserted.generated_assets += sqlx::query(
            "INSERT INTO generated_assets
             (id, campaign_session_id, turn_id, asset_kind, provider, model,
              location, prompt_fingerprint, metadata_json, created_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9::jsonb, $10::timestamptz)
             ON CONFLICT (id) DO NOTHING",
        )
        .bind(&row.state.id)
        .bind(&row.state.campaign_session_id)
        .bind(row.state.turn_id.as_deref())
        .bind(&row.state.asset_kind)
        .bind(&row.state.provider)
        .bind(&row.state.model)
        .bind(&row.state.location)
        .bind(row.state.prompt_fingerprint.as_deref())
        .bind(to_json(&row.state.metadata)?)
        .bind(&row.created_at)
        .execute(&mut *transaction)
        .await
        .map_err(LegacyImportError::Postgres)?
        .rows_affected();
    }

    let target_counts = target_counts(&mut transaction).await?;
    if target_counts != source_counts {
        return Err(LegacyImportError::TargetConflict);
    }
    let target_state = load_target_state(&mut transaction).await?;
    let target_state_digest = state_digest(&target_state)?;
    if target_state != source_state || target_state_digest != source_state_digest {
        return Err(LegacyImportError::TargetConflict);
    }
    let timestamp_match_count = verify_timestamps(&mut transaction, &snapshot).await?;
    let expected_timestamp_count = source_counts
        .campaign_sessions
        .checked_add(source_counts.characters)
        .and_then(|value| value.checked_add(source_counts.turn_audits))
        .and_then(|value| value.checked_add(source_counts.command_receipts))
        .and_then(|value| value.checked_add(source_counts.generated_assets))
        .ok_or(LegacyImportError::Invalid("legacy timestamp count"))?;
    if timestamp_match_count != expected_timestamp_count {
        return Err(LegacyImportError::TargetConflict);
    }
    transaction
        .commit()
        .await
        .map_err(LegacyImportError::Postgres)?;
    Ok(LegacyImportReport {
        schema_version: LEGACY_IMPORT_SCHEMA_VERSION,
        source_database_digest: snapshot.source_database_digest,
        source_migration_versions: snapshot.migration_versions,
        source_counts: source_counts.clone(),
        inserted_counts: inserted.clone(),
        already_present_counts: source_counts.checked_sub(&inserted)?,
        source_state_digest,
        target_state_digest,
        timestamp_match_count,
        committed: true,
    })
}

async fn load_snapshot(path: &Path) -> Result<LegacySnapshot, LegacyImportError> {
    let metadata = std::fs::symlink_metadata(path).map_err(LegacyImportError::Io)?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.len() == 0
        || metadata.len() > MAX_LEGACY_DATABASE_BYTES
    {
        return Err(LegacyImportError::Invalid("legacy database file"));
    }
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or(LegacyImportError::Invalid("legacy database path"))?;
    for suffix in ["-wal", "-shm"] {
        if path.with_file_name(format!("{name}{suffix}")).exists() {
            return Err(LegacyImportError::Invalid(
                "legacy database has live sidecar files",
            ));
        }
    }
    let source_database_digest = digest_file(path)?;
    let options = SqliteConnectOptions::new()
        .filename(path)
        .read_only(true)
        .immutable(true)
        .foreign_keys(true);
    let mut connection = SqliteConnection::connect_with(&options)
        .await
        .map_err(LegacyImportError::Sqlite)?;
    sqlx::query("PRAGMA query_only = ON")
        .execute(&mut connection)
        .await
        .map_err(LegacyImportError::Sqlite)?;
    let integrity: String = sqlx::query_scalar("PRAGMA integrity_check")
        .fetch_one(&mut connection)
        .await
        .map_err(LegacyImportError::Sqlite)?;
    if integrity != "ok" {
        return Err(LegacyImportError::Invalid("legacy database integrity"));
    }
    let migration_rows =
        sqlx::query("SELECT version, success FROM _sqlx_migrations ORDER BY version")
            .fetch_all(&mut connection)
            .await
            .map_err(LegacyImportError::Sqlite)?;
    let mut migration_versions = Vec::with_capacity(migration_rows.len());
    for row in migration_rows {
        let success: bool = row.try_get("success").map_err(LegacyImportError::Sqlite)?;
        if !success {
            return Err(LegacyImportError::Invalid("legacy migration state"));
        }
        migration_versions.push(row.try_get("version").map_err(LegacyImportError::Sqlite)?);
    }
    if migration_versions != [1, 2] {
        return Err(LegacyImportError::Invalid(
            "unsupported legacy migration version",
        ));
    }

    let campaigns = fetch_sqlite_rows(
        &mut connection,
        "SELECT id, schema_version, revision, payload_json, created_at, updated_at
         FROM campaign_sessions ORDER BY id",
        campaign_from_sqlite,
    )
    .await?;
    let characters = fetch_sqlite_rows(
        &mut connection,
        "SELECT id, campaign_session_id, schema_version, revision,
                payload_json, created_at, updated_at FROM characters ORDER BY id",
        character_from_sqlite,
    )
    .await?;
    let turns = fetch_sqlite_rows(
        &mut connection,
        "SELECT id, campaign_session_id, turn_number, actor_id,
                schema_version, payload_json, created_at
         FROM turn_audits ORDER BY campaign_session_id, turn_number, id",
        turn_from_sqlite,
    )
    .await?;
    let receipts = fetch_sqlite_rows(
        &mut connection,
        "SELECT campaign_session_id, idempotency_key, command_kind,
                request_fingerprint, expected_revision, result_revision,
                audit_id, response_json, created_at
         FROM command_receipts ORDER BY campaign_session_id, idempotency_key",
        receipt_from_sqlite,
    )
    .await?;
    let assets = fetch_sqlite_rows(
        &mut connection,
        "SELECT id, campaign_session_id, turn_id, asset_kind, provider, model,
                location, prompt_fingerprint, metadata_json, created_at
         FROM generated_assets ORDER BY campaign_session_id, created_at, id",
        asset_from_sqlite,
    )
    .await?;
    Ok(LegacySnapshot {
        source_database_digest,
        migration_versions,
        campaigns,
        characters,
        turns,
        receipts,
        assets,
    })
}

async fn fetch_sqlite_rows<T>(
    connection: &mut SqliteConnection,
    query: &str,
    convert: fn(SqliteRow) -> Result<T, LegacyImportError>,
) -> Result<Vec<T>, LegacyImportError> {
    sqlx::query(query)
        .fetch_all(connection)
        .await
        .map_err(LegacyImportError::Sqlite)?
        .into_iter()
        .map(convert)
        .collect()
}

fn validate_snapshot(snapshot: &LegacySnapshot) -> Result<(), LegacyImportError> {
    if snapshot.campaigns.is_empty()
        || snapshot.campaigns.len() > MAX_LEGACY_CAMPAIGNS
        || [
            snapshot.characters.len(),
            snapshot.turns.len(),
            snapshot.receipts.len(),
            snapshot.assets.len(),
        ]
        .into_iter()
        .any(|count| count > MAX_LEGACY_ROWS)
    {
        return Err(LegacyImportError::Invalid("legacy row count"));
    }
    let mut sessions = BTreeMap::new();
    for row in &snapshot.campaigns {
        validate_timestamp(&row.created_at)?;
        validate_timestamp(&row.updated_at)?;
        let session: SessionDto =
            serde_json::from_value(row.state.payload.clone()).map_err(LegacyImportError::Json)?;
        session
            .validate()
            .map_err(|_| LegacyImportError::Invalid("legacy campaign domain"))?;
        if row.state.id != session.id
            || row.state.schema_version != u32::from(session.schema_version)
            || row.state.schema_version != u32::from(SESSION_SCHEMA_VERSION)
            || row.state.revision != session.last_event_sequence.saturating_add(1)
            || sessions.insert(session.id.clone(), session).is_some()
        {
            return Err(LegacyImportError::Invalid("legacy campaign identity"));
        }
    }
    let mut supplied: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for row in &snapshot.characters {
        validate_timestamp(&row.created_at)?;
        validate_timestamp(&row.updated_at)?;
        let character: Character =
            serde_json::from_value(row.state.payload.clone()).map_err(LegacyImportError::Json)?;
        character
            .validate()
            .map_err(|_| LegacyImportError::Invalid("legacy character domain"))?;
        let campaign = row
            .state
            .campaign_session_id
            .as_deref()
            .ok_or(LegacyImportError::Invalid("orphan legacy character"))?;
        if row.state.id != character.id()
            || row.state.schema_version != CHARACTER_SCHEMA_VERSION
            || !sessions.contains_key(campaign)
            || !supplied
                .entry(campaign.to_owned())
                .or_default()
                .insert(character.id().to_owned())
        {
            return Err(LegacyImportError::Invalid("legacy character identity"));
        }
    }
    for (id, session) in &sessions {
        let declared = session
            .character_ids
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>();
        if declared != supplied.remove(id).unwrap_or_default() {
            return Err(LegacyImportError::Invalid("legacy campaign roster"));
        }
    }
    let mut audits = BTreeSet::new();
    for row in &snapshot.turns {
        validate_timestamp(&row.created_at)?;
        let event: SessionEventDto =
            serde_json::from_value(row.state.payload.clone()).map_err(LegacyImportError::Json)?;
        event
            .validate()
            .map_err(|_| LegacyImportError::Invalid("legacy turn domain"))?;
        if !is_valid_opaque_id(&row.state.id)
            || !sessions.contains_key(&row.state.campaign_session_id)
            || row.state.campaign_session_id != event.session_id
            || row.state.turn_number != event.sequence
            || row.state.schema_version != u32::from(event.schema_version)
            || !audits.insert((
                row.state.campaign_session_id.as_str(),
                row.state.id.as_str(),
            ))
        {
            return Err(LegacyImportError::Invalid("legacy turn identity"));
        }
    }
    for row in &snapshot.receipts {
        validate_timestamp(&row.created_at)?;
        if !sessions.contains_key(&row.state.campaign_session_id)
            || !is_valid_opaque_id(&row.state.idempotency_key)
            || Sha256Digest::new(row.state.request_fingerprint.clone()).is_err()
            || row.state.expected_revision == 0
            || row.state.result_revision != row.state.expected_revision.saturating_add(1)
            || !audits.contains(&(
                row.state.campaign_session_id.as_str(),
                row.state.audit_id.as_str(),
            ))
            || !row.state.response.is_object()
        {
            return Err(LegacyImportError::Invalid("legacy command receipt"));
        }
    }
    for row in &snapshot.assets {
        validate_timestamp(&row.created_at)?;
        if !is_valid_opaque_id(&row.state.id)
            || !sessions.contains_key(&row.state.campaign_session_id)
            || !bounded(&row.state.asset_kind, 128)
            || !bounded(&row.state.provider, 256)
            || !bounded(&row.state.model, 256)
            || !bounded(&row.state.location, 1024)
            || row
                .state
                .prompt_fingerprint
                .as_ref()
                .is_some_and(|value| Sha256Digest::new(value.clone()).is_err())
            || row.state.turn_id.as_ref().is_some_and(|turn| {
                !audits.contains(&(row.state.campaign_session_id.as_str(), turn.as_str()))
            })
            || !row.state.metadata.is_object()
        {
            return Err(LegacyImportError::Invalid("legacy generated asset"));
        }
    }
    Ok(())
}

async fn target_counts(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
) -> Result<LegacyImportCounts, LegacyImportError> {
    let row = sqlx::query(
        "SELECT (SELECT COUNT(*) FROM campaign_sessions)::bigint AS campaigns,
                (SELECT COUNT(*) FROM characters)::bigint AS characters,
                (SELECT COUNT(*) FROM turn_audits)::bigint AS turns,
                (SELECT COUNT(*) FROM command_receipts)::bigint AS receipts,
                (SELECT COUNT(*) FROM generated_assets)::bigint AS assets",
    )
    .fetch_one(&mut **transaction)
    .await
    .map_err(LegacyImportError::Postgres)?;
    Ok(LegacyImportCounts {
        campaign_sessions: pg_u64(&row, "campaigns")?,
        characters: pg_u64(&row, "characters")?,
        turn_audits: pg_u64(&row, "turns")?,
        command_receipts: pg_u64(&row, "receipts")?,
        generated_assets: pg_u64(&row, "assets")?,
    })
}

async fn load_target_state(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
) -> Result<LegacyState, LegacyImportError> {
    let campaigns = sqlx::query(
        "SELECT id, schema_version, revision, payload_json::text AS payload_json
         FROM campaign_sessions ORDER BY id",
    )
    .fetch_all(&mut **transaction)
    .await
    .map_err(LegacyImportError::Postgres)?
    .into_iter()
    .map(|row| {
        Ok(CampaignState {
            id: row.try_get("id").map_err(LegacyImportError::Postgres)?,
            schema_version: pg_u32(&row, "schema_version")?,
            revision: pg_u64(&row, "revision")?,
            payload: pg_json(&row, "payload_json")?,
        })
    })
    .collect::<Result<_, _>>()?;
    let characters = sqlx::query(
        "SELECT id, campaign_session_id, schema_version, revision,
                payload_json::text AS payload_json FROM characters ORDER BY id",
    )
    .fetch_all(&mut **transaction)
    .await
    .map_err(LegacyImportError::Postgres)?
    .into_iter()
    .map(|row| {
        Ok(CharacterState {
            id: row.try_get("id").map_err(LegacyImportError::Postgres)?,
            campaign_session_id: row
                .try_get("campaign_session_id")
                .map_err(LegacyImportError::Postgres)?,
            schema_version: pg_u32(&row, "schema_version")?,
            revision: pg_u64(&row, "revision")?,
            payload: pg_json(&row, "payload_json")?,
        })
    })
    .collect::<Result<_, _>>()?;
    let turns = sqlx::query(
        "SELECT id, campaign_session_id, turn_number, actor_id, schema_version,
                payload_json::text AS payload_json
         FROM turn_audits ORDER BY campaign_session_id, turn_number, id",
    )
    .fetch_all(&mut **transaction)
    .await
    .map_err(LegacyImportError::Postgres)?
    .into_iter()
    .map(|row| {
        Ok(TurnState {
            id: row.try_get("id").map_err(LegacyImportError::Postgres)?,
            campaign_session_id: row
                .try_get("campaign_session_id")
                .map_err(LegacyImportError::Postgres)?,
            turn_number: pg_u64(&row, "turn_number")?,
            actor_id: row
                .try_get("actor_id")
                .map_err(LegacyImportError::Postgres)?,
            schema_version: pg_u32(&row, "schema_version")?,
            payload: pg_json(&row, "payload_json")?,
        })
    })
    .collect::<Result<_, _>>()?;
    let receipts = sqlx::query(
        "SELECT campaign_session_id, idempotency_key, command_kind,
                request_fingerprint, expected_revision, result_revision,
                audit_id, response_json
         FROM command_receipts ORDER BY campaign_session_id, idempotency_key",
    )
    .fetch_all(&mut **transaction)
    .await
    .map_err(LegacyImportError::Postgres)?
    .into_iter()
    .map(|row| {
        Ok(ReceiptState {
            campaign_session_id: row
                .try_get("campaign_session_id")
                .map_err(LegacyImportError::Postgres)?,
            idempotency_key: row
                .try_get("idempotency_key")
                .map_err(LegacyImportError::Postgres)?,
            command_kind: row
                .try_get("command_kind")
                .map_err(LegacyImportError::Postgres)?,
            request_fingerprint: row
                .try_get("request_fingerprint")
                .map_err(LegacyImportError::Postgres)?,
            expected_revision: pg_u64(&row, "expected_revision")?,
            result_revision: pg_u64(&row, "result_revision")?,
            audit_id: row
                .try_get("audit_id")
                .map_err(LegacyImportError::Postgres)?,
            response: pg_json(&row, "response_json")?,
        })
    })
    .collect::<Result<_, _>>()?;
    let assets = sqlx::query(
        "SELECT id, campaign_session_id, turn_id, asset_kind, provider, model,
                location, prompt_fingerprint, metadata_json::text AS metadata_json
         FROM generated_assets ORDER BY campaign_session_id, created_at, id",
    )
    .fetch_all(&mut **transaction)
    .await
    .map_err(LegacyImportError::Postgres)?
    .into_iter()
    .map(|row| {
        Ok(AssetState {
            id: row.try_get("id").map_err(LegacyImportError::Postgres)?,
            campaign_session_id: row
                .try_get("campaign_session_id")
                .map_err(LegacyImportError::Postgres)?,
            turn_id: row
                .try_get("turn_id")
                .map_err(LegacyImportError::Postgres)?,
            asset_kind: row
                .try_get("asset_kind")
                .map_err(LegacyImportError::Postgres)?,
            provider: row
                .try_get("provider")
                .map_err(LegacyImportError::Postgres)?,
            model: row.try_get("model").map_err(LegacyImportError::Postgres)?,
            location: row
                .try_get("location")
                .map_err(LegacyImportError::Postgres)?,
            prompt_fingerprint: row
                .try_get("prompt_fingerprint")
                .map_err(LegacyImportError::Postgres)?,
            metadata: pg_json(&row, "metadata_json")?,
        })
    })
    .collect::<Result<_, _>>()?;
    Ok(LegacyState {
        campaigns,
        characters,
        turns,
        receipts,
        assets,
    })
}

async fn verify_timestamps(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    snapshot: &LegacySnapshot,
) -> Result<u64, LegacyImportError> {
    let mut matched = 0_u64;
    for row in &snapshot.campaigns {
        matched += u64::from(
            sqlx::query_scalar::<_, bool>(
                "SELECT created_at = $2::timestamptz AND updated_at = $3::timestamptz
                 FROM campaign_sessions WHERE id = $1",
            )
            .bind(&row.state.id)
            .bind(&row.created_at)
            .bind(&row.updated_at)
            .fetch_optional(&mut **transaction)
            .await
            .map_err(LegacyImportError::Postgres)?
            .unwrap_or(false),
        );
    }
    for row in &snapshot.characters {
        matched += u64::from(
            sqlx::query_scalar::<_, bool>(
                "SELECT created_at = $2::timestamptz AND updated_at = $3::timestamptz
                 FROM characters WHERE id = $1",
            )
            .bind(&row.state.id)
            .bind(&row.created_at)
            .bind(&row.updated_at)
            .fetch_optional(&mut **transaction)
            .await
            .map_err(LegacyImportError::Postgres)?
            .unwrap_or(false),
        );
    }
    for row in &snapshot.turns {
        matched += u64::from(
            one_timestamp(
                &mut *transaction,
                "turn_audits",
                &row.state.id,
                &row.created_at,
            )
            .await?,
        );
    }
    for row in &snapshot.receipts {
        let value: bool = sqlx::query_scalar(
            "SELECT created_at = $3::timestamptz FROM command_receipts
             WHERE campaign_session_id = $1 AND idempotency_key = $2",
        )
        .bind(&row.state.campaign_session_id)
        .bind(&row.state.idempotency_key)
        .bind(&row.created_at)
        .fetch_optional(&mut **transaction)
        .await
        .map_err(LegacyImportError::Postgres)?
        .unwrap_or(false);
        matched += u64::from(value);
    }
    for row in &snapshot.assets {
        matched += u64::from(
            one_timestamp(
                &mut *transaction,
                "generated_assets",
                &row.state.id,
                &row.created_at,
            )
            .await?,
        );
    }
    Ok(matched)
}

async fn one_timestamp(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    table: &'static str,
    id: &str,
    timestamp: &str,
) -> Result<bool, LegacyImportError> {
    let query = match table {
        "turn_audits" => "SELECT created_at = $2::timestamptz FROM turn_audits WHERE id = $1",
        "generated_assets" => {
            "SELECT created_at = $2::timestamptz FROM generated_assets WHERE id = $1"
        }
        _ => return Err(LegacyImportError::Invalid("legacy timestamp table")),
    };
    sqlx::query_scalar(query)
        .bind(id)
        .bind(timestamp)
        .fetch_optional(&mut **transaction)
        .await
        .map_err(LegacyImportError::Postgres)
        .map(|value| value.unwrap_or(false))
}

fn campaign_from_sqlite(row: SqliteRow) -> Result<CampaignRow, LegacyImportError> {
    Ok(CampaignRow {
        state: CampaignState {
            id: sqlite_string(&row, "id")?,
            schema_version: sqlite_u32(&row, "schema_version")?,
            revision: sqlite_u64(&row, "revision")?,
            payload: sqlite_json(&row, "payload_json")?,
        },
        created_at: sqlite_string(&row, "created_at")?,
        updated_at: sqlite_string(&row, "updated_at")?,
    })
}

fn character_from_sqlite(row: SqliteRow) -> Result<CharacterRow, LegacyImportError> {
    Ok(CharacterRow {
        state: CharacterState {
            id: sqlite_string(&row, "id")?,
            campaign_session_id: row
                .try_get("campaign_session_id")
                .map_err(LegacyImportError::Sqlite)?,
            schema_version: sqlite_u32(&row, "schema_version")?,
            revision: sqlite_u64(&row, "revision")?,
            payload: sqlite_json(&row, "payload_json")?,
        },
        created_at: sqlite_string(&row, "created_at")?,
        updated_at: sqlite_string(&row, "updated_at")?,
    })
}

fn turn_from_sqlite(row: SqliteRow) -> Result<TurnRow, LegacyImportError> {
    Ok(TurnRow {
        state: TurnState {
            id: sqlite_string(&row, "id")?,
            campaign_session_id: sqlite_string(&row, "campaign_session_id")?,
            turn_number: sqlite_u64(&row, "turn_number")?,
            actor_id: row.try_get("actor_id").map_err(LegacyImportError::Sqlite)?,
            schema_version: sqlite_u32(&row, "schema_version")?,
            payload: sqlite_json(&row, "payload_json")?,
        },
        created_at: sqlite_string(&row, "created_at")?,
    })
}

fn receipt_from_sqlite(row: SqliteRow) -> Result<ReceiptRow, LegacyImportError> {
    Ok(ReceiptRow {
        state: ReceiptState {
            campaign_session_id: sqlite_string(&row, "campaign_session_id")?,
            idempotency_key: sqlite_string(&row, "idempotency_key")?,
            command_kind: sqlite_string(&row, "command_kind")?,
            request_fingerprint: sqlite_string(&row, "request_fingerprint")?,
            expected_revision: sqlite_u64(&row, "expected_revision")?,
            result_revision: sqlite_u64(&row, "result_revision")?,
            audit_id: sqlite_string(&row, "audit_id")?,
            response: sqlite_json(&row, "response_json")?,
        },
        created_at: sqlite_string(&row, "created_at")?,
    })
}

fn asset_from_sqlite(row: SqliteRow) -> Result<AssetRow, LegacyImportError> {
    Ok(AssetRow {
        state: AssetState {
            id: sqlite_string(&row, "id")?,
            campaign_session_id: sqlite_string(&row, "campaign_session_id")?,
            turn_id: row.try_get("turn_id").map_err(LegacyImportError::Sqlite)?,
            asset_kind: sqlite_string(&row, "asset_kind")?,
            provider: sqlite_string(&row, "provider")?,
            model: sqlite_string(&row, "model")?,
            location: sqlite_string(&row, "location")?,
            prompt_fingerprint: row
                .try_get("prompt_fingerprint")
                .map_err(LegacyImportError::Sqlite)?,
            metadata: sqlite_json(&row, "metadata_json")?,
        },
        created_at: sqlite_string(&row, "created_at")?,
    })
}

fn sqlite_string(row: &SqliteRow, column: &str) -> Result<String, LegacyImportError> {
    row.try_get(column).map_err(LegacyImportError::Sqlite)
}

fn sqlite_u64(row: &SqliteRow, column: &str) -> Result<u64, LegacyImportError> {
    let value: i64 = row.try_get(column).map_err(LegacyImportError::Sqlite)?;
    value
        .try_into()
        .map_err(|_| LegacyImportError::Invalid("legacy numeric range"))
}

fn sqlite_u32(row: &SqliteRow, column: &str) -> Result<u32, LegacyImportError> {
    let value: i64 = row.try_get(column).map_err(LegacyImportError::Sqlite)?;
    value
        .try_into()
        .map_err(|_| LegacyImportError::Invalid("legacy numeric range"))
}

fn sqlite_json(row: &SqliteRow, column: &str) -> Result<Value, LegacyImportError> {
    let value: String = row.try_get(column).map_err(LegacyImportError::Sqlite)?;
    serde_json::from_str(&value).map_err(LegacyImportError::Json)
}

fn pg_u64(row: &sqlx::postgres::PgRow, column: &str) -> Result<u64, LegacyImportError> {
    let value: i64 = row.try_get(column).map_err(LegacyImportError::Postgres)?;
    value
        .try_into()
        .map_err(|_| LegacyImportError::Invalid("legacy numeric range"))
}

fn pg_u32(row: &sqlx::postgres::PgRow, column: &str) -> Result<u32, LegacyImportError> {
    let value: i64 = row.try_get(column).map_err(LegacyImportError::Postgres)?;
    value
        .try_into()
        .map_err(|_| LegacyImportError::Invalid("legacy numeric range"))
}

fn pg_json(row: &sqlx::postgres::PgRow, column: &str) -> Result<Value, LegacyImportError> {
    let value: String = row.try_get(column).map_err(LegacyImportError::Postgres)?;
    serde_json::from_str(&value).map_err(LegacyImportError::Json)
}

fn validate_timestamp(value: &str) -> Result<(), LegacyImportError> {
    if value.is_empty()
        || value.len() > 64
        || value.chars().any(char::is_control)
        || !value.ends_with('Z')
    {
        return Err(LegacyImportError::Invalid("legacy timestamp"));
    }
    Ok(())
}

fn bounded(value: &str, maximum: usize) -> bool {
    !value.is_empty() && value.len() <= maximum && !value.chars().any(char::is_control)
}

fn state_digest(state: &LegacyState) -> Result<Sha256Digest, LegacyImportError> {
    serde_json::to_vec(state)
        .map(|bytes| digest(&bytes))
        .map_err(LegacyImportError::Json)
}

fn digest_file(path: &Path) -> Result<Sha256Digest, LegacyImportError> {
    let mut file = File::open(path).map_err(LegacyImportError::Io)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 1024 * 1024];
    let mut length = 0_u64;
    loop {
        let read = file.read(&mut buffer).map_err(LegacyImportError::Io)?;
        if read == 0 {
            break;
        }
        length = length
            .checked_add(read as u64)
            .ok_or(LegacyImportError::Invalid("legacy database size"))?;
        if length > MAX_LEGACY_DATABASE_BYTES {
            return Err(LegacyImportError::Invalid("legacy database size"));
        }
        hasher.update(&buffer[..read]);
    }
    Ok(Sha256Digest::from_bytes(hasher.finalize().into()))
}

fn to_json(value: &Value) -> Result<String, LegacyImportError> {
    serde_json::to_string(value).map_err(LegacyImportError::Json)
}

fn to_i64(value: u64) -> Result<i64, LegacyImportError> {
    value
        .try_into()
        .map_err(|_| LegacyImportError::Invalid("legacy numeric range"))
}

fn usize_to_u64(value: usize) -> Result<u64, LegacyImportError> {
    value
        .try_into()
        .map_err(|_| LegacyImportError::Invalid("legacy row count"))
}

fn digest(bytes: &[u8]) -> Sha256Digest {
    Sha256Digest::from_bytes(Sha256::digest(bytes).into())
}
