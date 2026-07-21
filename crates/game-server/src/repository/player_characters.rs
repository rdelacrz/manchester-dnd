//! Parameterized PostgreSQL access for account-owned player characters.
//!
//! All methods take a server-derived `account_id`. No method accepts a
//! browser-provided owner ID. Cross-account access returns the same
//! `not_found` result as a missing character.

use manchester_dnd_core::{
    PLAYER_CHARACTER_SCHEMA_VERSION, PlayerCharacter,
    hero::{HeroChoices, HeroError},
    is_valid_opaque_id,
};
use serde::Serialize;
use sqlx::Row;

use super::PostgresRepository;
use crate::error::RepositoryError;

#[derive(Debug, Clone, Serialize)]
pub struct PlayerCharacterSummary {
    pub id: String,
    pub owner_account_id: String,
    pub revision: u64,
    pub display_name: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct PlayerCharacterDraftSummary {
    pub id: String,
    pub owner_account_id: String,
    pub revision: u64,
    pub expires_at: String,
    pub step: String,
    pub reviewed: bool,
    pub committed_character_id: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

impl PostgresRepository {
    /// Creates a new player character owned by `account_id`.
    /// The `account_id` must be server-derived; it is never accepted from
    /// browser input.
    pub async fn create_player_character(
        &self,
        account_id: &str,
        character: &PlayerCharacter,
    ) -> Result<PlayerCharacter, RepositoryError> {
        validate_account_id(account_id)?;
        validate_character_id(&character.character_id)?;
        if character.owner_account_id != account_id {
            return invalid(
                "player_character",
                &character.character_id,
                "character owner does not match the authenticated account",
            );
        }
        character.validate().map_err(|error| {
            to_repository_error("player_character", &character.character_id, &error)
        })?;
        let choices_json = serde_json::to_value(&character.choices).map_err(|error| {
            RepositoryError::Serialize {
                entity: "player_character",
                source: error,
            }
        })?;
        sqlx::query(
            "INSERT INTO player_characters
             (id, owner_account_id, revision, display_name, choices_json, schema_version)
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(&character.character_id)
        .bind(account_id)
        .bind(
            i64::try_from(character.revision)
                .map_err(|_| RepositoryError::NumericRange { field: "revision" })?,
        )
        .bind(&character.display_name)
        .bind(&choices_json)
        .bind(PLAYER_CHARACTER_SCHEMA_VERSION as i32)
        .execute(&self.pool)
        .await
        .map_err(|error| map_insert_error(error, "player_character", &character.character_id))?;
        Ok(PlayerCharacter {
            schema_version: character.schema_version,
            character_id: character.character_id.clone(),
            owner_account_id: account_id.to_owned(),
            revision: character.revision,
            display_name: character.display_name.clone(),
            choices: character.choices.clone(),
        })
    }

    /// Loads a player character by ID, scoped to `account_id`.
    /// Returns `None` if the character does not exist or is owned by a
    /// different account. This prevents cross-account enumeration.
    pub async fn load_player_character(
        &self,
        account_id: &str,
        character_id: &str,
    ) -> Result<Option<PlayerCharacter>, RepositoryError> {
        validate_account_id(account_id)?;
        validate_character_id(character_id)?;
        let row = sqlx::query(
            "SELECT id, owner_account_id, revision, display_name, choices_json, schema_version
             FROM player_characters
             WHERE id = $1 AND owner_account_id = $2",
        )
        .bind(character_id)
        .bind(account_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(RepositoryError::Database)?;
        row.map(|row| player_character_from_row(&row)).transpose()
    }

    /// Lists all player characters owned by `account_id`, sorted by
    /// most recently updated.
    pub async fn list_player_characters(
        &self,
        account_id: &str,
    ) -> Result<Vec<PlayerCharacterSummary>, RepositoryError> {
        validate_account_id(account_id)?;
        let rows = sqlx::query(
            "SELECT id, owner_account_id, revision, display_name,
                    created_at::text AS created_at, updated_at::text AS updated_at
             FROM player_characters
             WHERE owner_account_id = $1
             ORDER BY updated_at DESC, id",
        )
        .bind(account_id)
        .fetch_all(&self.pool)
        .await
        .map_err(RepositoryError::Database)?;
        rows.iter().map(player_character_summary_from_row).collect()
    }

    /// Updates a player character's display name, scoped to `account_id`.
    /// Uses optimistic revision control.
    pub async fn update_player_character_display_name(
        &self,
        account_id: &str,
        character_id: &str,
        expected_revision: u64,
        new_display_name: &str,
    ) -> Result<u64, RepositoryError> {
        validate_account_id(account_id)?;
        validate_character_id(character_id)?;
        if new_display_name.trim().is_empty()
            || new_display_name.chars().count() > 200
            || new_display_name.chars().any(char::is_control)
        {
            return invalid(
                "player_character",
                character_id,
                "display name must be 1-200 non-control characters",
            );
        }
        let result = sqlx::query(
            "UPDATE player_characters
             SET display_name = $3, revision = revision + 1, updated_at = CURRENT_TIMESTAMP
             WHERE id = $1 AND owner_account_id = $2 AND revision = $4",
        )
        .bind(character_id)
        .bind(account_id)
        .bind(new_display_name)
        .bind(
            i64::try_from(expected_revision)
                .map_err(|_| RepositoryError::NumericRange { field: "revision" })?,
        )
        .execute(&self.pool)
        .await
        .map_err(RepositoryError::Database)?;
        if result.rows_affected() == 0 {
            // Distinguish revision conflict from not-found: if the character
            // exists for this owner but with a different revision, return
            // RevisionConflict; otherwise return NotFound.
            let current: Option<i64> = sqlx::query_scalar(
                "SELECT revision FROM player_characters
                 WHERE id = $1 AND owner_account_id = $2",
            )
            .bind(character_id)
            .bind(account_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(RepositoryError::Database)?;
            return match current {
                Some(rev) => Err(RepositoryError::RevisionConflict {
                    entity: "player_character",
                    id: character_id.to_owned(),
                    expected: expected_revision,
                    actual: u64::try_from(rev).unwrap_or(0),
                }),
                None => Err(RepositoryError::NotFound {
                    entity: "player_character",
                    id: character_id.to_owned(),
                }),
            };
        }
        Ok(expected_revision + 1)
    }

    /// Deletes a player character, scoped to `account_id`.
    pub async fn delete_player_character(
        &self,
        account_id: &str,
        character_id: &str,
    ) -> Result<bool, RepositoryError> {
        validate_account_id(account_id)?;
        validate_character_id(character_id)?;
        let result =
            sqlx::query("DELETE FROM player_characters WHERE id = $1 AND owner_account_id = $2")
                .bind(character_id)
                .bind(account_id)
                .execute(&self.pool)
                .await
                .map_err(RepositoryError::Database)?;
        Ok(result.rows_affected() > 0)
    }

    // ── Drafts ──

    /// Creates a new character creation draft owned by `account_id`.
    pub async fn create_player_character_draft(
        &self,
        account_id: &str,
        draft_id: &str,
        expires_at_epoch_seconds: u64,
    ) -> Result<PlayerCharacterDraftSummary, RepositoryError> {
        validate_account_id(account_id)?;
        validate_draft_id(draft_id)?;
        let row = sqlx::query(
            "INSERT INTO player_character_drafts (id, owner_account_id, expires_at)
             VALUES ($1, $2, TO_TIMESTAMP($3))
             RETURNING id, owner_account_id, revision,
                       expires_at::text AS expires_at, step, reviewed,
                       committed_character_id, created_at::text AS created_at,
                       updated_at::text AS updated_at",
        )
        .bind(draft_id)
        .bind(account_id)
        .bind(i64::try_from(expires_at_epoch_seconds).map_err(|_| {
            RepositoryError::NumericRange {
                field: "expires_at",
            }
        })?)
        .fetch_one(&self.pool)
        .await
        .map_err(|error| map_insert_error(error, "player_character_draft", draft_id))?;
        draft_summary_from_row(&row)
    }

    /// Loads a draft by ID, scoped to `account_id`.
    pub async fn load_player_character_draft(
        &self,
        account_id: &str,
        draft_id: &str,
    ) -> Result<Option<(PlayerCharacterDraftSummary, Option<HeroChoices>)>, RepositoryError> {
        validate_account_id(account_id)?;
        validate_draft_id(draft_id)?;
        let row = sqlx::query(
            "SELECT id, owner_account_id, revision,
                    expires_at::text AS expires_at, step, reviewed,
                    committed_character_id, choices_json,
                    created_at::text AS created_at, updated_at::text AS updated_at
             FROM player_character_drafts
             WHERE id = $1 AND owner_account_id = $2",
        )
        .bind(draft_id)
        .bind(account_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(RepositoryError::Database)?;
        row.map(|row| {
            let summary = draft_summary_from_row(&row)?;
            let choices = row
                .try_get::<Option<serde_json::Value>, _>("choices_json")
                .map_err(RepositoryError::Database)?
                .map(|value| {
                    serde_json::from_value::<HeroChoices>(value).map_err(|error| {
                        RepositoryError::InvalidStoredData {
                            entity: "player_character_draft",
                            id: draft_id.to_owned(),
                            source: error,
                        }
                    })
                })
                .transpose()?;
            Ok((summary, choices))
        })
        .transpose()
    }

    /// Saves draft choices, scoped to `account_id`, with optimistic revision.
    pub async fn save_player_character_draft_choices(
        &self,
        account_id: &str,
        draft_id: &str,
        expected_revision: u64,
        choices: &HeroChoices,
        step: &str,
    ) -> Result<u64, RepositoryError> {
        validate_account_id(account_id)?;
        validate_draft_id(draft_id)?;
        choices
            .validate()
            .map_err(|error| to_repository_error("player_character_draft", draft_id, &error))?;
        let choices_json =
            serde_json::to_value(choices).map_err(|error| RepositoryError::Serialize {
                entity: "player_character_draft",
                source: error,
            })?;
        let result = sqlx::query(
            "UPDATE player_character_drafts
             SET choices_json = $3, step = $4, revision = revision + 1,
                 updated_at = CURRENT_TIMESTAMP
             WHERE id = $1 AND owner_account_id = $2 AND revision = $5",
        )
        .bind(draft_id)
        .bind(account_id)
        .bind(&choices_json)
        .bind(step)
        .bind(
            i64::try_from(expected_revision)
                .map_err(|_| RepositoryError::NumericRange { field: "revision" })?,
        )
        .execute(&self.pool)
        .await
        .map_err(RepositoryError::Database)?;
        if result.rows_affected() == 0 {
            return Err(RepositoryError::NotFound {
                entity: "player_character_draft",
                id: draft_id.to_owned(),
            });
        }
        Ok(expected_revision + 1)
    }

    /// Marks a draft as reviewed and links the committed character, scoped to
    /// `account_id`.
    pub async fn commit_player_character_draft(
        &self,
        account_id: &str,
        draft_id: &str,
        expected_revision: u64,
        character_id: &str,
    ) -> Result<u64, RepositoryError> {
        validate_account_id(account_id)?;
        validate_draft_id(draft_id)?;
        validate_character_id(character_id)?;
        let result = sqlx::query(
            "UPDATE player_character_drafts
             SET reviewed = TRUE, committed_character_id = $3,
                 revision = revision + 1, updated_at = CURRENT_TIMESTAMP
             WHERE id = $1 AND owner_account_id = $2 AND revision = $4
                AND reviewed = FALSE",
        )
        .bind(draft_id)
        .bind(account_id)
        .bind(character_id)
        .bind(
            i64::try_from(expected_revision)
                .map_err(|_| RepositoryError::NumericRange { field: "revision" })?,
        )
        .execute(&self.pool)
        .await
        .map_err(RepositoryError::Database)?;
        if result.rows_affected() == 0 {
            return Err(RepositoryError::NotFound {
                entity: "player_character_draft",
                id: draft_id.to_owned(),
            });
        }
        Ok(expected_revision + 1)
    }

    /// Deletes a draft, scoped to `account_id`.
    pub async fn delete_player_character_draft(
        &self,
        account_id: &str,
        draft_id: &str,
    ) -> Result<bool, RepositoryError> {
        validate_account_id(account_id)?;
        validate_draft_id(draft_id)?;
        let result = sqlx::query(
            "DELETE FROM player_character_drafts
             WHERE id = $1 AND owner_account_id = $2",
        )
        .bind(draft_id)
        .bind(account_id)
        .execute(&self.pool)
        .await
        .map_err(RepositoryError::Database)?;
        Ok(result.rows_affected() > 0)
    }

    /// Cleans up expired drafts. Returns the count of deleted rows.
    pub async fn cleanup_expired_player_character_drafts(&self) -> Result<u64, RepositoryError> {
        let result = sqlx::query(
            "DELETE FROM player_character_drafts WHERE expires_at <= CURRENT_TIMESTAMP",
        )
        .execute(&self.pool)
        .await
        .map_err(RepositoryError::Database)?;
        Ok(result.rows_affected())
    }

    // ── Immutable audits ──

    /// Appends an immutable audit row scoped to `account_id`. The caller is
    /// responsible for ensuring the audit JSON is canonical and complete. The
    /// `character_id` and `owner_account_id` are rebound server-side to prevent
    /// a stale or cross-account audit from being written.
    pub async fn insert_player_character_audit(
        &self,
        account_id: &str,
        character_id: &str,
        revision: u64,
        action: &str,
        audit_json: serde_json::Value,
    ) -> Result<(), RepositoryError> {
        validate_account_id(account_id)?;
        validate_character_id(character_id)?;
        validate_audit_action(action)?;
        if !audit_json.is_object() {
            return invalid(
                "player_character_audit",
                character_id,
                "audit payload must be a JSON object",
            );
        }
        sqlx::query(
            "INSERT INTO player_character_audits
             (character_id, owner_account_id, action, revision, audit_json)
             VALUES ($1, $2, $3, $4, $5::jsonb)",
        )
        .bind(character_id)
        .bind(account_id)
        .bind(action)
        .bind(
            i64::try_from(revision)
                .map_err(|_| RepositoryError::NumericRange { field: "revision" })?,
        )
        .bind(&audit_json)
        .execute(&self.pool)
        .await
        .map_err(|error| map_insert_error(error, "player_character_audit", character_id))?;
        Ok(())
    }

    // ── Idempotency receipts ──

    /// Loads an idempotency receipt scoped to `account_id` and `character_id`.
    /// Returns `None` if no receipt exists or if the receipt belongs to a
    /// different account/character pair. The unique key is
    /// `(character_id, idempotency_key)`; the `account_id` is an authorization
    /// scope that must match the receipt's stored owner.
    pub async fn load_player_character_command_receipt(
        &self,
        account_id: &str,
        character_id: &str,
        idempotency_key: &str,
    ) -> Result<Option<StoredPlayerCharacterReceipt>, RepositoryError> {
        validate_account_id(account_id)?;
        validate_receipt_entity_id(character_id)?;
        validate_idempotency_key(idempotency_key)?;
        let row = sqlx::query(
            "SELECT character_id, owner_account_id, idempotency_key, command_kind,
                    request_fingerprint, result_revision, response_json,
                    created_at::text AS created_at
             FROM player_character_command_receipts
             WHERE character_id = $1 AND idempotency_key = $2 AND owner_account_id = $3",
        )
        .bind(character_id)
        .bind(idempotency_key)
        .bind(account_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(RepositoryError::Database)?;
        row.map(player_character_receipt_from_row).transpose()
    }

    /// Inserts an idempotency receipt scoped to `account_id` and `character_id`.
    /// On a duplicate `(character_id, idempotency_key)` the unique constraint
    /// fires and is mapped to `AlreadyExists`. The caller must check for that
    /// and reload the existing receipt.
    pub async fn insert_player_character_command_receipt(
        &self,
        receipt: &NewPlayerCharacterReceipt<'_>,
    ) -> Result<(), RepositoryError> {
        validate_account_id(receipt.owner_account_id)?;
        validate_receipt_entity_id(receipt.character_id)?;
        validate_idempotency_key(receipt.idempotency_key)?;
        validate_command_kind(receipt.command_kind)?;
        validate_fingerprint(&receipt.request_fingerprint)?;
        sqlx::query(
            "INSERT INTO player_character_command_receipts
             (owner_account_id, character_id, idempotency_key, command_kind,
              request_fingerprint, result_revision, response_json)
             VALUES ($1, $2, $3, $4, $5, $6, $7::jsonb)",
        )
        .bind(receipt.owner_account_id)
        .bind(receipt.character_id)
        .bind(receipt.idempotency_key)
        .bind(receipt.command_kind)
        .bind(&receipt.request_fingerprint)
        .bind(i64::try_from(receipt.result_revision).map_err(|_| {
            RepositoryError::NumericRange {
                field: "result_revision",
            }
        })?)
        .bind(receipt.response_json.clone())
        .execute(&self.pool)
        .await
        .map_err(|error| {
            map_insert_error(
                error,
                "player_character_command_receipt",
                receipt.character_id,
            )
        })?;
        Ok(())
    }
}

/// A new idempotency receipt for a player character command. All fields are
/// server-derived; the `owner_account_id` is rebound from server authority.
#[derive(Debug, Clone)]
pub struct NewPlayerCharacterReceipt<'a> {
    pub owner_account_id: &'a str,
    pub character_id: &'a str,
    pub idempotency_key: &'a str,
    pub command_kind: &'a str,
    pub request_fingerprint: String,
    pub result_revision: u64,
    pub response_json: serde_json::Value,
}

/// A stored idempotency receipt loaded from the database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredPlayerCharacterReceipt {
    pub character_id: String,
    pub owner_account_id: String,
    pub idempotency_key: String,
    pub command_kind: String,
    pub request_fingerprint: String,
    pub result_revision: u64,
    pub response_json: serde_json::Value,
    pub created_at: String,
}

fn player_character_receipt_from_row(
    row: sqlx::postgres::PgRow,
) -> Result<StoredPlayerCharacterReceipt, RepositoryError> {
    let result_revision: i64 = row
        .try_get("result_revision")
        .map_err(RepositoryError::Database)?;
    let response_json: serde_json::Value = row
        .try_get("response_json")
        .map_err(RepositoryError::Database)?;
    Ok(StoredPlayerCharacterReceipt {
        character_id: row
            .try_get("character_id")
            .map_err(RepositoryError::Database)?,
        owner_account_id: row
            .try_get("owner_account_id")
            .map_err(RepositoryError::Database)?,
        idempotency_key: row
            .try_get("idempotency_key")
            .map_err(RepositoryError::Database)?,
        command_kind: row
            .try_get("command_kind")
            .map_err(RepositoryError::Database)?,
        request_fingerprint: row
            .try_get("request_fingerprint")
            .map_err(RepositoryError::Database)?,
        result_revision: u64::try_from(result_revision).map_err(|_| {
            RepositoryError::NumericRange {
                field: "result_revision",
            }
        })?,
        response_json,
        created_at: row
            .try_get("created_at")
            .map_err(RepositoryError::Database)?,
    })
}

fn validate_idempotency_key(key: &str) -> Result<(), RepositoryError> {
    // Matches the database CHECK: ^[a-zA-Z0-9_-]{1,128}$
    if key.is_empty()
        || key.len() > 128
        || !key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
    {
        return invalid(
            "player_character_command_receipt",
            key,
            "idempotency key must be 1-128 ASCII alphanumeric, underscore, or hyphen characters",
        );
    }
    Ok(())
}

fn validate_command_kind(kind: &str) -> Result<(), RepositoryError> {
    if kind.is_empty() || kind.len() > 64 {
        return invalid(
            "player_character_command_receipt",
            kind,
            "command kind must be 1-64 bytes",
        );
    }
    Ok(())
}

fn validate_fingerprint(fingerprint: &str) -> Result<(), RepositoryError> {
    if !fingerprint.starts_with("sha256:")
        || fingerprint.len() != 71
        || !fingerprint["sha256:".len()..]
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return invalid(
            "player_character_command_receipt",
            fingerprint,
            "request fingerprint must be a canonical sha256: hex digest",
        );
    }
    Ok(())
}

fn validate_audit_action(action: &str) -> Result<(), RepositoryError> {
    if action.is_empty() || action.len() > 64 {
        return invalid(
            "player_character_audit",
            action,
            "audit action must be 1-64 bytes",
        );
    }
    Ok(())
}

fn player_character_from_row(
    row: &sqlx::postgres::PgRow,
) -> Result<PlayerCharacter, RepositoryError> {
    let choices_json: serde_json::Value = row
        .try_get("choices_json")
        .map_err(RepositoryError::Database)?;
    let choices = serde_json::from_value::<HeroChoices>(choices_json).map_err(|error| {
        RepositoryError::InvalidStoredData {
            entity: "player_character",
            id: row.try_get::<String, _>("id").unwrap_or_default(),
            source: error,
        }
    })?;
    let schema_version: i32 = row
        .try_get("schema_version")
        .map_err(RepositoryError::Database)?;
    Ok(PlayerCharacter {
        schema_version: schema_version as u16,
        character_id: row.try_get("id").map_err(RepositoryError::Database)?,
        owner_account_id: row
            .try_get("owner_account_id")
            .map_err(RepositoryError::Database)?,
        revision: row
            .try_get::<i64, _>("revision")
            .map_err(RepositoryError::Database)? as u64,
        display_name: row
            .try_get("display_name")
            .map_err(RepositoryError::Database)?,
        choices,
    })
}

fn player_character_summary_from_row(
    row: &sqlx::postgres::PgRow,
) -> Result<PlayerCharacterSummary, RepositoryError> {
    Ok(PlayerCharacterSummary {
        id: row.try_get("id").map_err(RepositoryError::Database)?,
        owner_account_id: row
            .try_get("owner_account_id")
            .map_err(RepositoryError::Database)?,
        revision: row
            .try_get::<i64, _>("revision")
            .map_err(RepositoryError::Database)? as u64,
        display_name: row
            .try_get("display_name")
            .map_err(RepositoryError::Database)?,
        created_at: row
            .try_get("created_at")
            .map_err(RepositoryError::Database)?,
        updated_at: row
            .try_get("updated_at")
            .map_err(RepositoryError::Database)?,
    })
}

fn draft_summary_from_row(
    row: &sqlx::postgres::PgRow,
) -> Result<PlayerCharacterDraftSummary, RepositoryError> {
    Ok(PlayerCharacterDraftSummary {
        id: row.try_get("id").map_err(RepositoryError::Database)?,
        owner_account_id: row
            .try_get("owner_account_id")
            .map_err(RepositoryError::Database)?,
        revision: row
            .try_get::<i64, _>("revision")
            .map_err(RepositoryError::Database)? as u64,
        expires_at: row
            .try_get("expires_at")
            .map_err(RepositoryError::Database)?,
        step: row.try_get("step").map_err(RepositoryError::Database)?,
        reviewed: row.try_get("reviewed").map_err(RepositoryError::Database)?,
        committed_character_id: row
            .try_get("committed_character_id")
            .map_err(RepositoryError::Database)?,
        created_at: row
            .try_get("created_at")
            .map_err(RepositoryError::Database)?,
        updated_at: row
            .try_get("updated_at")
            .map_err(RepositoryError::Database)?,
    })
}

fn validate_account_id(account_id: &str) -> Result<(), RepositoryError> {
    if account_id == "account:local" {
        return Ok(());
    }
    if !account_id.starts_with("account:") || !is_valid_opaque_id(account_id) {
        return invalid("account", account_id, "account identifier is invalid");
    }
    Ok(())
}

fn validate_character_id(character_id: &str) -> Result<(), RepositoryError> {
    if character_id.starts_with("character:local-") {
        return Ok(());
    }
    if !character_id.starts_with("character:") || !is_valid_opaque_id(character_id) {
        return invalid(
            "player_character",
            character_id,
            "character identifier is invalid",
        );
    }
    Ok(())
}

fn validate_draft_id(draft_id: &str) -> Result<(), RepositoryError> {
    if !draft_id.starts_with("draft:") || !is_valid_opaque_id(draft_id) {
        return invalid(
            "player_character_draft",
            draft_id,
            "draft identifier is invalid",
        );
    }
    Ok(())
}

/// Validates an entity ID used in idempotency receipts. Both `character:` and
/// `draft:` prefixed IDs are accepted because draft mutations also write
/// receipts scoped to the draft ID.
fn validate_receipt_entity_id(entity_id: &str) -> Result<(), RepositoryError> {
    if (entity_id.starts_with("character:") || entity_id.starts_with("draft:"))
        && is_valid_opaque_id(entity_id)
    {
        return Ok(());
    }
    if entity_id.starts_with("character:local-") {
        return Ok(());
    }
    invalid(
        "player_character_command_receipt",
        entity_id,
        "entity identifier is invalid",
    )
}

fn to_repository_error(entity: &'static str, id: &str, _error: &HeroError) -> RepositoryError {
    RepositoryError::InvalidDomainState {
        entity,
        id: id.to_owned(),
        reason: "failed hero-domain validation",
    }
}

#[allow(clippy::collapsible_if)]
fn map_insert_error(error: sqlx::Error, entity: &'static str, id: &str) -> RepositoryError {
    if let sqlx::Error::Database(db_error) = &error {
        if let Some(code) = db_error.code().as_deref() {
            match code {
                "23505" => {
                    return RepositoryError::AlreadyExists {
                        entity,
                        id: id.to_owned(),
                    };
                }
                "23503" => {
                    return RepositoryError::InvalidDomainState {
                        entity,
                        id: id.to_owned(),
                        reason: "referenced account does not exist",
                    };
                }
                _ => {}
            }
        }
    }
    RepositoryError::Database(error)
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
    use super::*;
    use crate::repository::MIGRATOR;
    use manchester_dnd_core::hero::{
        AncestryId, BackgroundId, BackgroundSelection, ClassSelection, EquipmentId,
        EquipmentSelection, FightingStyleId, HeroChoices, HeroConceptId, HeroPins,
        HeroPresentation, SkillId, StandardArrayAssignment, ThemeId,
    };
    use sqlx::PgPool;
    use uuid::Uuid;

    fn test_choices(theme: ThemeId) -> HeroChoices {
        HeroChoices {
            pins: HeroPins::mvp(theme),
            concept: HeroConceptId::CanalGuardian,
            ancestry: AncestryId::Human,
            class: ClassSelection::Fighter {
                fighting_style: FightingStyleId::Defense,
            },
            ability_assignment: StandardArrayAssignment {
                strength: 15,
                dexterity: 14,
                constitution: 13,
                intelligence: 12,
                wisdom: 10,
                charisma: 8,
            },
            background: BackgroundSelection {
                background: BackgroundId::Soldier,
                class_skills: vec![SkillId::Perception, SkillId::Survival],
            },
            equipment: EquipmentSelection {
                carried: vec![
                    EquipmentId::Longsword,
                    EquipmentId::LightCrossbow,
                    EquipmentId::ChainMail,
                    EquipmentId::ExplorersPack,
                ],
                simple_weapon: None,
                equipped_armor: Some(EquipmentId::ChainMail),
                shield_equipped: false,
            },
            wizard_spells: None,
            presentation: HeroPresentation {
                name: "Test Hero".to_owned(),
                pronouns: "they/them".to_owned(),
                appearance: "A weathered adventurer".to_owned(),
                ideal: "Justice for all".to_owned(),
                bond: "Owes a life debt".to_owned(),
                flaw: "Too trusting".to_owned(),
                tone_limits: Vec::new(),
            },
        }
    }

    fn new_character(account_id: &str, name: &str) -> PlayerCharacter {
        PlayerCharacter::new(
            format!("character:{}", Uuid::new_v4()),
            account_id.to_owned(),
            name.to_owned(),
            test_choices(ThemeId::RainboundBorough),
        )
        .expect("test character should be valid")
    }

    async fn insert_test_account(pool: &PgPool, account_id: &str) {
        sqlx::query(
            "INSERT INTO accounts (id, display_name, login_enabled) VALUES ($1, $2, FALSE)",
        )
        .bind(account_id)
        .bind("Test Account")
        .execute(pool)
        .await
        .expect("test account should insert");
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn create_load_list_and_delete_player_character(pool: PgPool) {
        let account = "account:aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
        insert_test_account(&pool, account).await;
        let repo = PostgresRepository::from_pool(pool);
        let character = new_character(account, "First Hero");

        let created = repo
            .create_player_character(account, &character)
            .await
            .expect("create should succeed");
        assert_eq!(created.character_id, character.character_id);
        assert_eq!(created.owner_account_id, account);

        let loaded = repo
            .load_player_character(account, &character.character_id)
            .await
            .expect("load should succeed")
            .expect("character should exist");
        assert_eq!(loaded.display_name, "First Hero");
        assert_eq!(loaded.choices, character.choices);

        let listed = repo
            .list_player_characters(account)
            .await
            .expect("list should succeed");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].display_name, "First Hero");

        assert!(
            repo.delete_player_character(account, &character.character_id)
                .await
                .expect("delete should succeed")
        );
        assert!(
            repo.load_player_character(account, &character.character_id)
                .await
                .expect("load should succeed")
                .is_none()
        );
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn cross_account_access_returns_not_found(pool: PgPool) {
        let account_a = "account:aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
        let account_b = "account:bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
        insert_test_account(&pool, account_a).await;
        insert_test_account(&pool, account_b).await;
        let repo = PostgresRepository::from_pool(pool);
        let character = new_character(account_a, "Account A Hero");

        repo.create_player_character(account_a, &character)
            .await
            .expect("create should succeed");

        // Account B cannot load account A's character.
        let result = repo
            .load_player_character(account_b, &character.character_id)
            .await
            .expect("query should succeed");
        assert!(result.is_none(), "cross-account load should return None");

        // Account B cannot list account A's characters.
        let listed = repo
            .list_player_characters(account_b)
            .await
            .expect("list should succeed");
        assert!(listed.is_empty(), "cross-account list should be empty");

        // Account B cannot delete account A's character.
        assert!(
            !repo
                .delete_player_character(account_b, &character.character_id)
                .await
                .expect("delete should succeed"),
            "cross-account delete should return false"
        );

        // Account A can still load the character.
        assert!(
            repo.load_player_character(account_a, &character.character_id)
                .await
                .expect("load should succeed")
                .is_some()
        );
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn update_display_name_with_optimistic_revision(pool: PgPool) {
        let account = "account:aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
        insert_test_account(&pool, account).await;
        let repo = PostgresRepository::from_pool(pool);
        let character = new_character(account, "Original Name");
        repo.create_player_character(account, &character)
            .await
            .expect("create should succeed");

        let new_revision = repo
            .update_player_character_display_name(account, &character.character_id, 0, "New Name")
            .await
            .expect("update should succeed");
        assert_eq!(new_revision, 1);

        let loaded = repo
            .load_player_character(account, &character.character_id)
            .await
            .expect("load should succeed")
            .expect("character should exist");
        assert_eq!(loaded.display_name, "New Name");
        assert_eq!(loaded.revision, 1);

        // Stale revision fails.
        let result = repo
            .update_player_character_display_name(account, &character.character_id, 0, "Stale")
            .await;
        assert!(result.is_err());
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn draft_create_load_save_commit_and_delete(pool: PgPool) {
        let account = "account:aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
        insert_test_account(&pool, account).await;
        let repo = PostgresRepository::from_pool(pool);
        let draft_id = format!("draft:{}", Uuid::new_v4());

        let draft = repo
            .create_player_character_draft(account, &draft_id, 9999999999)
            .await
            .expect("create draft should succeed");
        assert_eq!(draft.step, "campaign_theme");
        assert!(!draft.reviewed);

        let (loaded, choices) = repo
            .load_player_character_draft(account, &draft_id)
            .await
            .expect("load should succeed")
            .expect("draft should exist");
        assert_eq!(loaded.id, draft_id);
        assert!(choices.is_none());

        let choices = test_choices(ThemeId::RainboundBorough);
        let new_rev = repo
            .save_player_character_draft_choices(account, &draft_id, 0, &choices, "review")
            .await
            .expect("save should succeed");
        assert_eq!(new_rev, 1);

        let (_, loaded_choices) = repo
            .load_player_character_draft(account, &draft_id)
            .await
            .expect("load should succeed")
            .expect("draft should exist");
        assert!(loaded_choices.is_some());

        let character_id = format!("character:{}", Uuid::new_v4());
        let character = PlayerCharacter::new(
            character_id.clone(),
            account.to_owned(),
            "Draft Hero".to_owned(),
            test_choices(ThemeId::RainboundBorough),
        )
        .expect("character should be valid");
        repo.create_player_character(account, &character)
            .await
            .expect("create character should succeed");
        repo.commit_player_character_draft(account, &draft_id, 1, &character_id)
            .await
            .expect("commit should succeed");

        let (committed, _) = repo
            .load_player_character_draft(account, &draft_id)
            .await
            .expect("load should succeed")
            .expect("draft should exist");
        assert!(committed.reviewed);
        assert_eq!(committed.committed_character_id, Some(character_id));

        assert!(
            repo.delete_player_character_draft(account, &draft_id)
                .await
                .expect("delete should succeed")
        );
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn cross_account_draft_access_returns_not_found(pool: PgPool) {
        let account_a = "account:aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
        let account_b = "account:bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
        insert_test_account(&pool, account_a).await;
        insert_test_account(&pool, account_b).await;
        let repo = PostgresRepository::from_pool(pool);
        let draft_id = format!("draft:{}", Uuid::new_v4());

        repo.create_player_character_draft(account_a, &draft_id, 9999999999)
            .await
            .expect("create should succeed");

        // Account B cannot load account A's draft.
        let result = repo
            .load_player_character_draft(account_b, &draft_id)
            .await
            .expect("query should succeed");
        assert!(
            result.is_none(),
            "cross-account draft load should return None"
        );

        // Account B cannot delete account A's draft.
        assert!(
            !repo
                .delete_player_character_draft(account_b, &draft_id)
                .await
                .expect("delete should succeed"),
            "cross-account draft delete should return false"
        );
    }
}
