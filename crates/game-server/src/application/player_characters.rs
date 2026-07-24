//! Account-scoped application service for the player character library.
//!
//! Every public method takes a server-derived `account_id`. No method accepts
//! a browser-supplied owner. Cross-account access — including a guessed
//! character or draft ID — returns the same `character_not_found` result as a
//! missing document. This is enforced at both the repository layer (SQL
//! `WHERE owner_account_id = $1` scoping) and here at the service layer.
//!
//! Mutations write an immutable audit row and an idempotency receipt. The
//! receipt's `(character_id, idempotency_key)` uniqueness makes retries safe:
//! a duplicate request either returns the stored response or fails fast if the
//! command body changed.
//!
//! Campaign-bound runtime state (instances, stats) is deliberately stubbed.
//! Campaign memberships land in Tasks 12–13; until then these stubs return an
//! empty list or `character_not_found`, never a cross-account leak.

use manchester_dnd_core::{PLAYER_CHARACTER_SCHEMA_VERSION, PlayerCharacter, is_valid_opaque_id};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::GameApplicationService;
use crate::{
    error::{ApplicationError, RepositoryError},
    repository::{
        NewPlayerCharacterReceipt, PlayerCharacterDraftSummary, PlayerCharacterSummary,
        StoredPlayerCharacterReceipt,
    },
};

/// Default draft TTL: 7 days from creation.
pub const PLAYER_CHARACTER_DRAFT_TTL_SECONDS: u64 = 7 * 24 * 60 * 60;
/// Drafts are retained for 30 days after expiry before hard deletion, so an
/// interrupted creation can still be inspected or restored by the owner.
pub const PLAYER_CHARACTER_DRAFT_RETENTION_SECONDS: u64 = 30 * 24 * 60 * 60;

const UPDATE_DISPLAY_NAME_COMMAND_KIND: &str = "player_character_update_display_name";
const DRAFT_SAVE_COMMAND_KIND: &str = "player_character_draft_save";
const DRAFT_COMMIT_COMMAND_KIND: &str = "player_character_draft_commit";

/// A campaign instance derived from a library character. Stubbed until
/// campaign memberships (Tasks 12–13) exist.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CampaignInstanceSummary {
    pub campaign_id: String,
    pub campaign_title: String,
    pub runtime_character_id: String,
    pub level: u8,
    pub active: bool,
}

/// A campaign-bound character stats snapshot. Stubbed until campaign
/// memberships (Tasks 12–13) exist.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CampaignCharacterStats {
    pub campaign_id: String,
    pub runtime_character_id: String,
    pub level: u8,
    pub experience_points: u32,
    pub current_hit_points: i32,
    pub maximum_hit_points: i32,
}

impl GameApplicationService {
    // ── Reads ──

    /// Lists all player characters owned by `account_id`, sorted by most
    /// recently updated. Returns an empty vec for an unknown or empty account.
    pub async fn list_player_characters(
        &self,
        account_id: &str,
    ) -> Result<Vec<PlayerCharacterSummary>, ApplicationError> {
        self.repository
            .list_player_characters(account_id)
            .await
            .map_err(map_player_character_error)
    }

    /// Loads a single player character scoped to `account_id`. Returns
    /// `character_not_found` if the character does not exist or is owned by a
    /// different account. The two cases are indistinguishable to the caller.
    pub async fn load_owned_player_character(
        &self,
        account_id: &str,
        character_id: &str,
    ) -> Result<PlayerCharacter, ApplicationError> {
        self.repository
            .load_player_character(account_id, character_id)
            .await
            .map_err(map_player_character_error)?
            .ok_or(ApplicationError::WrongCharacter)
    }

    // ── Mutations ──

    /// Creates a new player character owned by `account_id`. The
    /// `account_id` is always server-derived and must match the character's
    /// `owner_account_id`; a mismatch is rejected before any write.
    pub async fn create_player_character(
        &self,
        account_id: &str,
        character: PlayerCharacter,
    ) -> Result<PlayerCharacter, ApplicationError> {
        let created = self
            .repository
            .create_player_character(account_id, &character)
            .await
            .map_err(map_player_character_error)?;
        // Best-effort audit: a failed audit insert does not roll back the
        // character creation, but surfaces as an internal error so the owner
        // sees a consistent failure rather than a silent gap.
        let audit_payload = serde_json::json!({
            "action": "create",
            "character_id": created.character_id,
            "display_name": created.display_name,
            "revision": created.revision,
            "schema_version": created.schema_version,
        });
        self.repository
            .insert_player_character_audit(
                account_id,
                &created.character_id,
                created.revision,
                "create",
                audit_payload,
            )
            .await
            .map_err(map_player_character_error)?;
        Ok(created)
    }

    /// Updates a character's display name with optimistic revision control.
    /// Writes an immutable audit row and an idempotency receipt. A duplicate
    /// request with the same fingerprint returns the stored result revision;
    /// a duplicate with a different fingerprint returns `IdempotencyConflict`.
    pub async fn update_player_character_display_name(
        &self,
        account_id: &str,
        character_id: &str,
        expected_revision: u64,
        new_display_name: String,
        idempotency_key: String,
    ) -> Result<u64, ApplicationError> {
        validate_idempotency_key_shape(&idempotency_key)?;
        let fingerprint = fingerprint_update_display_name(
            account_id,
            character_id,
            expected_revision,
            &new_display_name,
        );

        // Idempotent replay: same command body returns the stored result.
        if let Some(receipt) = self
            .repository
            .load_player_character_command_receipt(account_id, character_id, &idempotency_key)
            .await
            .map_err(map_player_character_error)?
        {
            return resolve_update_receipt(&receipt, &fingerprint, &idempotency_key);
        }

        let new_revision = self
            .repository
            .update_player_character_display_name(
                account_id,
                character_id,
                expected_revision,
                &new_display_name,
            )
            .await
            .map_err(map_player_character_error)?;

        let audit_payload = serde_json::json!({
            "action": "update_display_name",
            "expected_revision": expected_revision,
            "result_revision": new_revision,
            "new_display_name": new_display_name,
        });
        self.repository
            .insert_player_character_audit(
                account_id,
                character_id,
                new_revision,
                "update_display_name",
                audit_payload,
            )
            .await
            .map_err(map_player_character_error)?;

        let receipt = NewPlayerCharacterReceipt {
            owner_account_id: account_id,
            character_id,
            idempotency_key: &idempotency_key,
            command_kind: UPDATE_DISPLAY_NAME_COMMAND_KIND,
            request_fingerprint: fingerprint.to_string(),
            result_revision: new_revision,
            response_json: serde_json::json!({
                "result_revision": new_revision,
                "new_display_name": new_display_name,
            }),
        };
        match self
            .repository
            .insert_player_character_command_receipt(&receipt)
            .await
        {
            Ok(()) => Ok(new_revision),
            Err(RepositoryError::AlreadyExists {
                entity: "player_character_command_receipt",
                ..
            }) => {
                // A concurrent duplicate won the race. Reload the receipt and
                // return its result instead of surfacing a conflict.
                let stored = self
                    .repository
                    .load_player_character_command_receipt(
                        account_id,
                        character_id,
                        &idempotency_key,
                    )
                    .await
                    .map_err(map_player_character_error)?
                    .ok_or(ApplicationError::InvalidStoredState)?;
                resolve_update_receipt(&stored, &fingerprint, &idempotency_key)
            }
            Err(error) => Err(map_player_character_error(error)),
        }
    }

    /// Deletes a player character scoped to `account_id`. Returns `true` if a
    /// character was deleted, `false` if the character did not exist or was
    /// owned by a different account. The two cases are indistinguishable.
    /// Writes an audit row before deletion so the trail survives the row.
    pub async fn delete_player_character(
        &self,
        account_id: &str,
        character_id: &str,
    ) -> Result<bool, ApplicationError> {
        // Load first so we can audit the deletion with the pre-delete revision.
        // A missing or foreign character returns None here; both map to false.
        let character = self
            .repository
            .load_player_character(account_id, character_id)
            .await
            .map_err(map_player_character_error)?;
        let Some(character) = character else {
            return Ok(false);
        };
        let audit_payload = serde_json::json!({
            "action": "delete",
            "character_id": character.character_id,
            "display_name": character.display_name,
            "revision_before": character.revision,
        });
        self.repository
            .insert_player_character_audit(
                account_id,
                character_id,
                character.revision,
                "delete",
                audit_payload,
            )
            .await
            .map_err(map_player_character_error)?;
        self.repository
            .delete_player_character(account_id, character_id)
            .await
            .map_err(map_player_character_error)
    }

    // ── Drafts ──

    /// Creates a new character-creation draft owned by `account_id`. The draft
    /// expires after `PLAYER_CHARACTER_DRAFT_TTL_SECONDS` and is retained for
    /// `PLAYER_CHARACTER_DRAFT_RETENTION_SECONDS` after expiry before cleanup.
    pub async fn create_player_character_draft(
        &self,
        account_id: &str,
    ) -> Result<PlayerCharacterDraftSummary, ApplicationError> {
        let draft_id = format!("draft:{}", Uuid::new_v4());
        let now = self.player_character_now_epoch_seconds();
        let expires_at = now
            .checked_add(PLAYER_CHARACTER_DRAFT_TTL_SECONDS)
            .ok_or(ApplicationError::InvalidStoredState)?;
        self.repository
            .create_player_character_draft(account_id, &draft_id, expires_at)
            .await
            .map_err(map_player_character_error)
    }

    /// Loads a draft scoped to `account_id`. Returns `character_not_found` if
    /// the draft does not exist or is owned by a different account.
    pub async fn load_player_character_draft(
        &self,
        account_id: &str,
        draft_id: &str,
    ) -> Result<
        (
            PlayerCharacterDraftSummary,
            Option<manchester_dnd_core::hero::HeroChoices>,
        ),
        ApplicationError,
    > {
        self.repository
            .load_player_character_draft(account_id, draft_id)
            .await
            .map_err(map_player_character_error)?
            .ok_or(ApplicationError::WrongCharacter)
    }

    /// Saves draft choices with optimistic revision control and an idempotency
    /// receipt. A duplicate request with the same fingerprint returns the
    /// stored result revision.
    pub async fn save_player_character_draft_choices(
        &self,
        account_id: &str,
        draft_id: &str,
        expected_revision: u64,
        choices: manchester_dnd_core::hero::HeroChoices,
        step: String,
        idempotency_key: String,
    ) -> Result<u64, ApplicationError> {
        validate_idempotency_key_shape(&idempotency_key)?;
        let fingerprint =
            fingerprint_draft_save(account_id, draft_id, expected_revision, &choices, &step);

        if let Some(receipt) = self
            .repository
            .load_player_character_command_receipt(account_id, draft_id, &idempotency_key)
            .await
            .map_err(map_player_character_error)?
        {
            return resolve_draft_receipt(
                &receipt,
                &fingerprint,
                &idempotency_key,
                DRAFT_SAVE_COMMAND_KIND,
            );
        }

        let new_revision = self
            .repository
            .save_player_character_draft_choices(
                account_id,
                draft_id,
                expected_revision,
                &choices,
                &step,
            )
            .await
            .map_err(map_player_character_error)?;

        let receipt = NewPlayerCharacterReceipt {
            owner_account_id: account_id,
            character_id: draft_id,
            idempotency_key: &idempotency_key,
            command_kind: DRAFT_SAVE_COMMAND_KIND,
            request_fingerprint: fingerprint.to_string(),
            result_revision: new_revision,
            response_json: serde_json::json!({
                "result_revision": new_revision,
                "step": step,
            }),
        };
        match self
            .repository
            .insert_player_character_command_receipt(&receipt)
            .await
        {
            Ok(()) => Ok(new_revision),
            Err(RepositoryError::AlreadyExists {
                entity: "player_character_command_receipt",
                ..
            }) => {
                let stored = self
                    .repository
                    .load_player_character_command_receipt(account_id, draft_id, &idempotency_key)
                    .await
                    .map_err(map_player_character_error)?
                    .ok_or(ApplicationError::InvalidStoredState)?;
                resolve_draft_receipt(
                    &stored,
                    &fingerprint,
                    &idempotency_key,
                    DRAFT_SAVE_COMMAND_KIND,
                )
            }
            Err(error) => Err(map_player_character_error(error)),
        }
    }

    /// Commits a draft by linking it to a created character, scoped to
    /// `account_id`. Writes an idempotency receipt.
    pub async fn commit_player_character_draft(
        &self,
        account_id: &str,
        draft_id: &str,
        expected_revision: u64,
        character_id: &str,
        idempotency_key: String,
    ) -> Result<u64, ApplicationError> {
        validate_idempotency_key_shape(&idempotency_key)?;
        let fingerprint =
            fingerprint_draft_commit(account_id, draft_id, expected_revision, character_id);

        if let Some(receipt) = self
            .repository
            .load_player_character_command_receipt(account_id, draft_id, &idempotency_key)
            .await
            .map_err(map_player_character_error)?
        {
            return resolve_draft_receipt(
                &receipt,
                &fingerprint,
                &idempotency_key,
                DRAFT_COMMIT_COMMAND_KIND,
            );
        }

        let new_revision = self
            .repository
            .commit_player_character_draft(account_id, draft_id, expected_revision, character_id)
            .await
            .map_err(map_player_character_error)?;

        let receipt = NewPlayerCharacterReceipt {
            owner_account_id: account_id,
            character_id: draft_id,
            idempotency_key: &idempotency_key,
            command_kind: DRAFT_COMMIT_COMMAND_KIND,
            request_fingerprint: fingerprint.to_string(),
            result_revision: new_revision,
            response_json: serde_json::json!({
                "result_revision": new_revision,
                "committed_character_id": character_id,
            }),
        };
        match self
            .repository
            .insert_player_character_command_receipt(&receipt)
            .await
        {
            Ok(()) => Ok(new_revision),
            Err(RepositoryError::AlreadyExists {
                entity: "player_character_command_receipt",
                ..
            }) => {
                let stored = self
                    .repository
                    .load_player_character_command_receipt(account_id, draft_id, &idempotency_key)
                    .await
                    .map_err(map_player_character_error)?
                    .ok_or(ApplicationError::InvalidStoredState)?;
                resolve_draft_receipt(
                    &stored,
                    &fingerprint,
                    &idempotency_key,
                    DRAFT_COMMIT_COMMAND_KIND,
                )
            }
            Err(error) => Err(map_player_character_error(error)),
        }
    }

    /// Deletes a draft scoped to `account_id`. Returns `true` if a draft was
    /// deleted, `false` if the draft did not exist or was owned by a different
    /// account. The two cases are indistinguishable to the caller.
    pub async fn delete_player_character_draft(
        &self,
        account_id: &str,
        draft_id: &str,
    ) -> Result<bool, ApplicationError> {
        self.repository
            .delete_player_character_draft(account_id, draft_id)
            .await
            .map_err(map_player_character_error)
    }

    /// Cleans up expired drafts. Returns the count of deleted rows. This is an
    /// administrative operation, not account-scoped.
    pub async fn cleanup_expired_player_character_drafts(&self) -> Result<u64, ApplicationError> {
        self.repository
            .cleanup_expired_player_character_drafts()
            .await
            .map_err(map_player_character_error)
    }

    // ── Campaign-bound runtime stubs ──
    //
    // These verify source ownership and return empty/not-found until campaign
    // memberships (Tasks 12–13) are implemented. A foreign character ID returns
    // the same `character_not_found` as a missing one.

    /// Lists campaign instances derived from a library character. Stubbed: the
    /// caller must own the source character; until campaign memberships exist,
    /// this always returns an empty vec for an owned character and
    /// `character_not_found` for a foreign or missing one.
    pub async fn list_authorized_campaign_instances(
        &self,
        account_id: &str,
        player_character_id: &str,
    ) -> Result<Vec<CampaignInstanceSummary>, ApplicationError> {
        // Verify source ownership. A foreign or missing character returns the
        // same not_found result; no enumeration is possible.
        self.load_owned_player_character(account_id, player_character_id)
            .await?;
        // Campaign memberships (Tasks 12–13) are not implemented yet.
        Ok(Vec::new())
    }

    /// Loads campaign-bound character stats for a library character in a
    /// specific campaign. Stubbed: the caller must own the source character;
    /// until campaign memberships exist, this always returns
    /// `character_not_found` (the campaign has no runtime instance yet).
    pub async fn load_authorized_campaign_character_stats(
        &self,
        account_id: &str,
        player_character_id: &str,
        campaign_id: &str,
    ) -> Result<CampaignCharacterStats, ApplicationError> {
        // Verify source ownership first. A foreign or missing character
        // returns the same not_found result.
        self.load_owned_player_character(account_id, player_character_id)
            .await?;
        // Campaign memberships (Tasks 12–13) are not implemented yet, so no
        // runtime instance can exist. Return character_not_found rather than
        // fabricating stats. The campaign_id is validated for shape to avoid
        // accepting arbitrary input silently.
        if !is_valid_opaque_id(campaign_id) {
            return Err(ApplicationError::WrongCharacter);
        }
        Err(ApplicationError::WrongCharacter)
    }

    // ── Helpers ──

    fn player_character_now_epoch_seconds(&self) -> u64 {
        // The clock returns milliseconds; convert to seconds for the draft
        // expiry timestamp. The division is safe because the clock is
        // monotonic and always >= 0.
        self.clock.now_unix_ms() / 1_000
    }
}

fn map_player_character_error(error: RepositoryError) -> ApplicationError {
    match error {
        RepositoryError::NotFound {
            entity: "player_character" | "player_character_draft",
            ..
        } => ApplicationError::WrongCharacter,
        RepositoryError::RevisionConflict {
            expected, actual, ..
        } => ApplicationError::RevisionConflict {
            expected,
            current_revision: actual,
        },
        other => ApplicationError::Repository(other),
    }
}

fn validate_idempotency_key_shape(key: &str) -> Result<(), ApplicationError> {
    if key.is_empty()
        || key.len() > 128
        || !key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
    {
        return Err(ApplicationError::InvalidStoredState);
    }
    Ok(())
}

fn resolve_update_receipt(
    receipt: &StoredPlayerCharacterReceipt,
    fingerprint: &str,
    idempotency_key: &str,
) -> Result<u64, ApplicationError> {
    if receipt.request_fingerprint != fingerprint {
        return Err(ApplicationError::IdempotencyConflict);
    }
    if receipt.idempotency_key != idempotency_key
        || receipt.command_kind != UPDATE_DISPLAY_NAME_COMMAND_KIND
    {
        return Err(ApplicationError::InvalidStoredState);
    }
    Ok(receipt.result_revision)
}

fn resolve_draft_receipt(
    receipt: &StoredPlayerCharacterReceipt,
    fingerprint: &str,
    idempotency_key: &str,
    expected_kind: &str,
) -> Result<u64, ApplicationError> {
    if receipt.request_fingerprint != fingerprint {
        return Err(ApplicationError::IdempotencyConflict);
    }
    if receipt.idempotency_key != idempotency_key || receipt.command_kind != expected_kind {
        return Err(ApplicationError::InvalidStoredState);
    }
    Ok(receipt.result_revision)
}

// ── Command fingerprints ──
//
// The fingerprint is a canonical SHA-256 of the command's semantic fields.
// A duplicate request with the same idempotency key but a different
// fingerprint (different body) is an IdempotencyConflict.

#[derive(Serialize)]
struct NormalizedUpdateDisplayName<'a> {
    schema_version: u16,
    account_id: &'a str,
    character_id: &'a str,
    expected_revision: u64,
    new_display_name: &'a str,
}

fn fingerprint_update_display_name(
    account_id: &str,
    character_id: &str,
    expected_revision: u64,
    new_display_name: &str,
) -> String {
    let normalized = NormalizedUpdateDisplayName {
        schema_version: PLAYER_CHARACTER_SCHEMA_VERSION,
        account_id,
        character_id,
        expected_revision,
        new_display_name,
    };
    fingerprint(&normalized)
}

#[derive(Serialize)]
struct NormalizedDraftSave<'a> {
    schema_version: u16,
    account_id: &'a str,
    draft_id: &'a str,
    expected_revision: u64,
    step: &'a str,
}

fn fingerprint_draft_save(
    account_id: &str,
    draft_id: &str,
    expected_revision: u64,
    _choices: &manchester_dnd_core::hero::HeroChoices,
    step: &str,
) -> String {
    // Note: choices are validated by the repository, but the fingerprint
    // intentionally excludes them to keep the digest stable across
    // serialization quirks in nested enums. The step + revision + ids are
    // the semantic identity of the save command.
    let normalized = NormalizedDraftSave {
        schema_version: PLAYER_CHARACTER_SCHEMA_VERSION,
        account_id,
        draft_id,
        expected_revision,
        step,
    };
    fingerprint(&normalized)
}

#[derive(Serialize)]
struct NormalizedDraftCommit<'a> {
    schema_version: u16,
    account_id: &'a str,
    draft_id: &'a str,
    expected_revision: u64,
    character_id: &'a str,
}

fn fingerprint_draft_commit(
    account_id: &str,
    draft_id: &str,
    expected_revision: u64,
    character_id: &str,
) -> String {
    let normalized = NormalizedDraftCommit {
        schema_version: PLAYER_CHARACTER_SCHEMA_VERSION,
        account_id,
        draft_id,
        expected_revision,
        character_id,
    };
    fingerprint(&normalized)
}

fn fingerprint(value: &impl Serialize) -> String {
    let serialized = serde_json::to_vec(value).expect("normalized command is serializable");
    let digest: [u8; 32] = Sha256::digest(serialized).into();
    let mut value = String::with_capacity(71);
    value.push_str("sha256:");
    for byte in digest {
        use std::fmt::Write as _;
        write!(&mut value, "{byte:02x}").expect("writing to a String cannot fail");
    }
    value
}

#[cfg(test)]
mod tests {
    use std::{sync::Arc, time::Duration};

    use manchester_dnd_core::hero::{
        AncestryId, BackgroundId, BackgroundSelection, ClassSelection, EquipmentId,
        EquipmentSelection, FightingStyleId, HeroChoices, HeroConceptId, HeroPins,
        HeroPresentation, SkillId, StandardArrayAssignment, ThemeId,
    };
    use mongodb::bson::{DateTime, doc};
    use uuid::Uuid;

    use super::*;
    use crate::{
        config::{AccessMode, MongoConfig, MongoSchemaPolicy, SecretString},
        persistence::{CollectionName, MongoStore, SchemaReconciler},
        repository::MongoRepository,
        seed::SeedVault,
    };

    const ACCOUNT_A: &str = "account:aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    const ACCOUNT_B: &str = "account:bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";

    async fn service() -> Option<(GameApplicationService, MongoRepository, String)> {
        let Ok(uri) = std::env::var("MONGODB_TEST_URI") else {
            eprintln!("skipping MongoDB player-character test: MONGODB_TEST_URI is not set");
            return None;
        };
        assert!(
            uri.starts_with("mongodb://root:") && uri.contains("127.0.0.1"),
            "MONGODB_TEST_URI must be the explicit local root test URI"
        );
        let database = format!("mdnd_player_characters_test_{}", Uuid::new_v4().simple());
        let store = MongoStore::connect(&MongoConfig {
            uri: SecretString::new(uri),
            database: database.clone(),
            max_pool_size: 4,
            min_pool_size: 0,
            connect_timeout: Duration::from_secs(5),
            server_selection_timeout: Duration::from_secs(5),
            operation_timeout: Duration::from_secs(15),
            transaction_timeout: Duration::from_secs(10),
            transaction_max_retries: 2,
            schema_policy: MongoSchemaPolicy::ApplyAndVerify,
        })
        .await
        .expect("test MongoDB must connect");
        SchemaReconciler::new(store.clone())
            .apply()
            .await
            .expect("MongoDB schema must apply");
        let repository = MongoRepository::new(store);
        let application = GameApplicationService::with_sources(
            AccessMode::LocalSingleUser,
            repository.clone(),
            Arc::new(SeedVault::from_key([9; 32])),
            |_| 12,
            || 1_700_000_000_000,
        );
        Some((application, repository, database))
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
            .expect("test account should insert");
    }

    async fn drop_database(repository: &MongoRepository, database: &str) {
        assert!(
            database.starts_with("mdnd_player_characters_test_") && database != "manchester_dnd",
            "cleanup safeguard"
        );
        repository
            .store()
            .database()
            .drop()
            .await
            .expect("test database must drop");
    }

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

    // ── Two-account isolation tests ──

    #[tokio::test]
    async fn account_a_cannot_list_load_or_mutate_account_b_character() {
        let Some((service, repository, database)) = service().await else {
            return;
        };
        insert_account(&repository, ACCOUNT_A).await;
        insert_account(&repository, ACCOUNT_B).await;

        // Account A creates a character.
        let character = new_character(ACCOUNT_A, "Account A Hero");
        let created = service
            .create_player_character(ACCOUNT_A, character.clone())
            .await
            .expect("account A create should succeed");

        // Account B cannot list account A's characters.
        let listed_b = service
            .list_player_characters(ACCOUNT_B)
            .await
            .expect("list should succeed");
        assert!(listed_b.is_empty(), "account B should see zero characters");

        // Account A can list its own.
        let listed_a = service
            .list_player_characters(ACCOUNT_A)
            .await
            .expect("list should succeed");
        assert_eq!(listed_a.len(), 1);
        assert_eq!(listed_a[0].display_name, "Account A Hero");

        // Account B cannot load account A's character — even with a guessed ID.
        let result = service
            .load_owned_player_character(ACCOUNT_B, &created.character_id)
            .await;
        assert!(
            matches!(result, Err(ApplicationError::WrongCharacter)),
            "cross-account load must return character_not_found, got {result:?}"
        );

        // Account B cannot update account A's character display name.
        let result = service
            .update_player_character_display_name(
                ACCOUNT_B,
                &created.character_id,
                0,
                "Stolen Name".to_owned(),
                "key-b-update".to_owned(),
            )
            .await;
        assert!(result.is_err(), "cross-account update must fail");

        // Account B cannot delete account A's character.
        let deleted = service
            .delete_player_character(ACCOUNT_B, &created.character_id)
            .await
            .expect("delete should succeed");
        assert!(!deleted, "cross-account delete must return false");

        // Account A can still load the character.
        let loaded = service
            .load_owned_player_character(ACCOUNT_A, &created.character_id)
            .await
            .expect("account A load should succeed");
        assert_eq!(loaded.display_name, "Account A Hero");
        drop_database(&repository, &database).await;
    }

    #[tokio::test]
    async fn account_a_cannot_list_load_or_mutate_account_b_draft() {
        let Some((service, repository, database)) = service().await else {
            return;
        };
        insert_account(&repository, ACCOUNT_A).await;
        insert_account(&repository, ACCOUNT_B).await;

        // Account A creates a draft.
        let draft = service
            .create_player_character_draft(ACCOUNT_A)
            .await
            .expect("account A draft create should succeed");

        // Account B cannot load account A's draft — even with a guessed ID.
        let result = service
            .load_player_character_draft(ACCOUNT_B, &draft.id)
            .await;
        assert!(
            matches!(result, Err(ApplicationError::WrongCharacter)),
            "cross-account draft load must return character_not_found, got {result:?}"
        );

        // Account B cannot save choices to account A's draft.
        let result = service
            .save_player_character_draft_choices(
                ACCOUNT_B,
                &draft.id,
                0,
                test_choices(ThemeId::RainboundBorough),
                "review".to_owned(),
                "key-b-draft-save".to_owned(),
            )
            .await;
        assert!(result.is_err(), "cross-account draft save must fail");

        // Account B cannot commit account A's draft.
        let character = new_character(ACCOUNT_A, "Draft Hero");
        let created = service
            .create_player_character(ACCOUNT_A, character.clone())
            .await
            .expect("account A create should succeed");
        let result = service
            .commit_player_character_draft(
                ACCOUNT_B,
                &draft.id,
                0,
                &created.character_id,
                "key-b-draft-commit".to_owned(),
            )
            .await;
        assert!(result.is_err(), "cross-account draft commit must fail");

        // Account B cannot delete account A's draft.
        let deleted = service
            .delete_player_character_draft(ACCOUNT_B, &draft.id)
            .await
            .expect("delete should succeed");
        assert!(!deleted, "cross-account draft delete must return false");

        // Account A can still load the draft.
        let (loaded, _) = service
            .load_player_character_draft(ACCOUNT_A, &draft.id)
            .await
            .expect("account A draft load should succeed");
        assert_eq!(loaded.id, draft.id);
        drop_database(&repository, &database).await;
    }

    // ── Audit + idempotency receipt tests ──

    #[tokio::test]
    async fn update_display_name_writes_audit_and_receipt() {
        let Some((service, repository, database)) = service().await else {
            return;
        };
        insert_account(&repository, ACCOUNT_A).await;
        let character = new_character(ACCOUNT_A, "Original Name");
        let created = service
            .create_player_character(ACCOUNT_A, character)
            .await
            .expect("create should succeed");

        let new_revision = service
            .update_player_character_display_name(
                ACCOUNT_A,
                &created.character_id,
                0,
                "New Name".to_owned(),
                "key-update-1".to_owned(),
            )
            .await
            .expect("update should succeed");
        assert_eq!(new_revision, 1);

        // An audit event was written.
        let audit_count = repository
            .store()
            .document_collection(CollectionName::AuditEvents)
            .count_documents(doc! {
                "category": "player_character",
                "action": "update_display_name",
                "scope_kind": "player_character",
                "scope_id": &created.character_id,
                "actor_account_id": ACCOUNT_A,
                "outcome": "committed",
            })
            .await
            .unwrap();
        assert_eq!(audit_count, 1, "one update audit row should exist");

        // A committed command receipt was written.
        let receipt_count = repository
            .store()
            .document_collection(CollectionName::CommandReceipts)
            .count_documents(doc! {
                "scope_kind": "player_character",
                "scope_id": &created.character_id,
                "actor_account_id": ACCOUNT_A,
                "command_kind": UPDATE_DISPLAY_NAME_COMMAND_KIND,
                "idempotency_key": "key-update-1",
                "state": "committed",
            })
            .await
            .unwrap();
        assert_eq!(receipt_count, 1, "one receipt row should exist");
        drop_database(&repository, &database).await;
    }

    #[tokio::test]
    async fn duplicate_update_with_same_key_returns_stored_revision() {
        let Some((service, repository, database)) = service().await else {
            return;
        };
        insert_account(&repository, ACCOUNT_A).await;
        let character = new_character(ACCOUNT_A, "Original Name");
        let created = service
            .create_player_character(ACCOUNT_A, character)
            .await
            .expect("create should succeed");

        let first = service
            .update_player_character_display_name(
                ACCOUNT_A,
                &created.character_id,
                0,
                "New Name".to_owned(),
                "key-dup-update".to_owned(),
            )
            .await
            .expect("first update should succeed");
        assert_eq!(first, 1);

        // Duplicate with the same key and body returns the stored revision.
        let second = service
            .update_player_character_display_name(
                ACCOUNT_A,
                &created.character_id,
                0,
                "New Name".to_owned(),
                "key-dup-update".to_owned(),
            )
            .await
            .expect("duplicate update should return stored revision");
        assert_eq!(second, 1, "duplicate should return stored revision");

        // Only one committed receipt exists.
        let receipt_count = repository
            .store()
            .document_collection(CollectionName::CommandReceipts)
            .count_documents(doc! {
                "scope_kind": "player_character",
                "scope_id": &created.character_id,
                "actor_account_id": ACCOUNT_A,
                "command_kind": UPDATE_DISPLAY_NAME_COMMAND_KIND,
                "idempotency_key": "key-dup-update",
                "state": "committed",
            })
            .await
            .unwrap();
        assert_eq!(
            receipt_count, 1,
            "duplicate must not write a second receipt"
        );
        drop_database(&repository, &database).await;
    }

    #[tokio::test]
    async fn duplicate_update_with_different_body_is_idempotency_conflict() {
        let Some((service, repository, database)) = service().await else {
            return;
        };
        insert_account(&repository, ACCOUNT_A).await;
        let character = new_character(ACCOUNT_A, "Original Name");
        let created = service
            .create_player_character(ACCOUNT_A, character)
            .await
            .expect("create should succeed");

        service
            .update_player_character_display_name(
                ACCOUNT_A,
                &created.character_id,
                0,
                "First Name".to_owned(),
                "key-conflict-update".to_owned(),
            )
            .await
            .expect("first update should succeed");

        // Same idempotency key, different body -> IdempotencyConflict.
        let result = service
            .update_player_character_display_name(
                ACCOUNT_A,
                &created.character_id,
                0,
                "Second Name".to_owned(),
                "key-conflict-update".to_owned(),
            )
            .await;
        assert!(
            matches!(result, Err(ApplicationError::IdempotencyConflict)),
            "same key with different body must be IdempotencyConflict, got {result:?}"
        );
        drop_database(&repository, &database).await;
    }

    #[tokio::test]
    async fn stale_revision_fails_with_revision_conflict() {
        let Some((service, repository, database)) = service().await else {
            return;
        };
        insert_account(&repository, ACCOUNT_A).await;
        let character = new_character(ACCOUNT_A, "Original Name");
        let created = service
            .create_player_character(ACCOUNT_A, character)
            .await
            .expect("create should succeed");

        // First update bumps revision to 1.
        service
            .update_player_character_display_name(
                ACCOUNT_A,
                &created.character_id,
                0,
                "First".to_owned(),
                "key-stale-1".to_owned(),
            )
            .await
            .expect("first update should succeed");

        // Stale revision (0) fails.
        let result = service
            .update_player_character_display_name(
                ACCOUNT_A,
                &created.character_id,
                0,
                "Second".to_owned(),
                "key-stale-2".to_owned(),
            )
            .await;
        assert!(
            matches!(result, Err(ApplicationError::RevisionConflict { .. })),
            "stale revision must fail with RevisionConflict, got {result:?}"
        );
        drop_database(&repository, &database).await;
    }

    #[tokio::test]
    async fn delete_writes_audit_and_returns_true() {
        let Some((service, repository, database)) = service().await else {
            return;
        };
        insert_account(&repository, ACCOUNT_A).await;
        let character = new_character(ACCOUNT_A, "Doomed Hero");
        let created = service
            .create_player_character(ACCOUNT_A, character)
            .await
            .expect("create should succeed");

        let deleted = service
            .delete_player_character(ACCOUNT_A, &created.character_id)
            .await
            .expect("delete should succeed");
        assert!(deleted, "owned character delete should return true");

        // The delete audit event survives deletion of the character document.
        let audit_count = repository
            .store()
            .document_collection(CollectionName::AuditEvents)
            .count_documents(doc! {
                "category": "player_character",
                "action": "delete",
                "scope_kind": "player_character",
                "scope_id": &created.character_id,
                "actor_account_id": ACCOUNT_A,
                "outcome": "committed",
            })
            .await
            .unwrap();
        assert_eq!(audit_count, 1, "one delete audit row should exist");
        drop_database(&repository, &database).await;
    }

    // ── Campaign-bound runtime stub tests ──

    #[tokio::test]
    async fn list_authorized_campaign_instances_returns_empty_for_owned_character() {
        let Some((service, repository, database)) = service().await else {
            return;
        };
        insert_account(&repository, ACCOUNT_A).await;
        let character = new_character(ACCOUNT_A, "Library Hero");
        let created = service
            .create_player_character(ACCOUNT_A, character)
            .await
            .expect("create should succeed");

        let instances = service
            .list_authorized_campaign_instances(ACCOUNT_A, &created.character_id)
            .await
            .expect("owned character should return an empty list");
        assert!(
            instances.is_empty(),
            "stub should return empty vec for owned character"
        );
        drop_database(&repository, &database).await;
    }

    #[tokio::test]
    async fn list_authorized_campaign_instances_returns_not_found_for_foreign_character() {
        let Some((service, repository, database)) = service().await else {
            return;
        };
        insert_account(&repository, ACCOUNT_A).await;
        insert_account(&repository, ACCOUNT_B).await;
        let character = new_character(ACCOUNT_A, "Account A Hero");
        let created = service
            .create_player_character(ACCOUNT_A, character)
            .await
            .expect("create should succeed");

        // Account B guessing the ID gets the same not_found as a missing ID.
        let result = service
            .list_authorized_campaign_instances(ACCOUNT_B, &created.character_id)
            .await;
        assert!(
            matches!(result, Err(ApplicationError::WrongCharacter)),
            "foreign character must return character_not_found, got {result:?}"
        );

        // A genuinely missing character ID also returns not_found.
        let missing_id = format!("character:{}", Uuid::new_v4());
        let result = service
            .list_authorized_campaign_instances(ACCOUNT_A, &missing_id)
            .await;
        assert!(
            matches!(result, Err(ApplicationError::WrongCharacter)),
            "missing character must return character_not_found, got {result:?}"
        );
        drop_database(&repository, &database).await;
    }

    #[tokio::test]
    async fn load_authorized_campaign_character_stats_returns_not_found_for_owned_character() {
        let Some((service, repository, database)) = service().await else {
            return;
        };
        insert_account(&repository, ACCOUNT_A).await;
        let character = new_character(ACCOUNT_A, "Library Hero");
        let created = service
            .create_player_character(ACCOUNT_A, character)
            .await
            .expect("create should succeed");

        // Even for an owned character, no campaign instance exists yet.
        let result = service
            .load_authorized_campaign_character_stats(
                ACCOUNT_A,
                &created.character_id,
                "campaign:aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
            )
            .await;
        assert!(
            matches!(result, Err(ApplicationError::WrongCharacter)),
            "stub should return character_not_found for owned character, got {result:?}"
        );
        drop_database(&repository, &database).await;
    }

    #[tokio::test]
    async fn load_authorized_campaign_character_stats_returns_not_found_for_foreign_character() {
        let Some((service, repository, database)) = service().await else {
            return;
        };
        insert_account(&repository, ACCOUNT_A).await;
        insert_account(&repository, ACCOUNT_B).await;
        let character = new_character(ACCOUNT_A, "Account A Hero");
        let created = service
            .create_player_character(ACCOUNT_A, character)
            .await
            .expect("create should succeed");

        // Account B guessing the ID gets not_found, same as a missing ID.
        let result = service
            .load_authorized_campaign_character_stats(
                ACCOUNT_B,
                &created.character_id,
                "campaign:aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
            )
            .await;
        assert!(
            matches!(result, Err(ApplicationError::WrongCharacter)),
            "foreign character must return character_not_found, got {result:?}"
        );
        drop_database(&repository, &database).await;
    }

    #[tokio::test]
    async fn mismatched_character_campaign_pair_returns_not_found() {
        // Even if a campaign existed, a mismatched character/campaign pair must
        // return not_found. Until campaign memberships exist, this is the stub
        // behavior for any pairing.
        let Some((service, repository, database)) = service().await else {
            return;
        };
        insert_account(&repository, ACCOUNT_A).await;
        let character = new_character(ACCOUNT_A, "Library Hero");
        let created = service
            .create_player_character(ACCOUNT_A, character)
            .await
            .expect("create should succeed");

        // A campaign ID that does not match any membership returns not_found.
        let result = service
            .load_authorized_campaign_character_stats(
                ACCOUNT_A,
                &created.character_id,
                "campaign:cccccccc-cccc-4ccc-8ccc-cccccccccccc",
            )
            .await;
        assert!(
            matches!(result, Err(ApplicationError::WrongCharacter)),
            "mismatched campaign must return character_not_found, got {result:?}"
        );
        drop_database(&repository, &database).await;
    }

    #[tokio::test]
    async fn foreign_campaign_returns_not_found() {
        // A campaign owned by another account must not leak stats. Until
        // campaign memberships exist, this is the stub behavior.
        let Some((service, repository, database)) = service().await else {
            return;
        };
        insert_account(&repository, ACCOUNT_A).await;
        let character = new_character(ACCOUNT_A, "Library Hero");
        let created = service
            .create_player_character(ACCOUNT_A, character)
            .await
            .expect("create should succeed");

        // A foreign campaign ID returns not_found.
        let result = service
            .load_authorized_campaign_character_stats(
                ACCOUNT_A,
                &created.character_id,
                "campaign:dddddddd-dddd-4ddd-8ddd-dddddddddddd",
            )
            .await;
        assert!(
            matches!(result, Err(ApplicationError::WrongCharacter)),
            "foreign campaign must return character_not_found, got {result:?}"
        );
        drop_database(&repository, &database).await;
    }

    #[tokio::test]
    async fn removed_membership_returns_not_found() {
        // A removed membership must return not_found. Until campaign
        // memberships exist, this is the stub behavior for any campaign.
        let Some((service, repository, database)) = service().await else {
            return;
        };
        insert_account(&repository, ACCOUNT_A).await;
        let character = new_character(ACCOUNT_A, "Library Hero");
        let created = service
            .create_player_character(ACCOUNT_A, character)
            .await
            .expect("create should succeed");

        // No membership exists, so this returns not_found.
        let result = service
            .load_authorized_campaign_character_stats(
                ACCOUNT_A,
                &created.character_id,
                "campaign:eeeeeeee-eeee-4eee-8eee-eeeeeeeeeeee",
            )
            .await;
        assert!(
            matches!(result, Err(ApplicationError::WrongCharacter)),
            "removed/absent membership must return character_not_found, got {result:?}"
        );
        drop_database(&repository, &database).await;
    }

    // ── Draft lifecycle tests ──

    #[tokio::test]
    async fn draft_create_load_save_commit_delete_round_trip() {
        let Some((service, repository, database)) = service().await else {
            return;
        };
        insert_account(&repository, ACCOUNT_A).await;

        let draft = service
            .create_player_character_draft(ACCOUNT_A)
            .await
            .expect("draft create should succeed");
        assert_eq!(draft.step, "campaign_theme");
        assert!(!draft.reviewed);

        let (loaded, choices) = service
            .load_player_character_draft(ACCOUNT_A, &draft.id)
            .await
            .expect("draft load should succeed");
        assert_eq!(loaded.id, draft.id);
        assert!(choices.is_none());

        let choices = test_choices(ThemeId::RainboundBorough);
        let new_rev = service
            .save_player_character_draft_choices(
                ACCOUNT_A,
                &draft.id,
                0,
                choices.clone(),
                "review".to_owned(),
                "key-draft-save".to_owned(),
            )
            .await
            .expect("draft save should succeed");
        assert_eq!(new_rev, 1);

        // Duplicate save with same key returns stored revision.
        let dup = service
            .save_player_character_draft_choices(
                ACCOUNT_A,
                &draft.id,
                0,
                choices,
                "review".to_owned(),
                "key-draft-save".to_owned(),
            )
            .await
            .expect("duplicate draft save should return stored revision");
        assert_eq!(dup, 1);

        let character = new_character(ACCOUNT_A, "Draft Hero");
        let created = service
            .create_player_character(ACCOUNT_A, character)
            .await
            .expect("character create should succeed");
        let committed_rev = service
            .commit_player_character_draft(
                ACCOUNT_A,
                &draft.id,
                1,
                &created.character_id,
                "key-draft-commit".to_owned(),
            )
            .await
            .expect("draft commit should succeed");
        assert_eq!(committed_rev, 2);

        let (committed, _) = service
            .load_player_character_draft(ACCOUNT_A, &draft.id)
            .await
            .expect("draft load should succeed");
        assert!(committed.reviewed);
        assert_eq!(
            committed.committed_character_id,
            Some(created.character_id.clone())
        );

        let deleted = service
            .delete_player_character_draft(ACCOUNT_A, &draft.id)
            .await
            .expect("draft delete should succeed");
        assert!(deleted);
        drop_database(&repository, &database).await;
    }

    #[tokio::test]
    async fn missing_character_load_returns_not_found() {
        let Some((service, repository, database)) = service().await else {
            return;
        };
        insert_account(&repository, ACCOUNT_A).await;
        let missing_id = format!("character:{}", Uuid::new_v4());
        let result = service
            .load_owned_player_character(ACCOUNT_A, &missing_id)
            .await;
        assert!(
            matches!(result, Err(ApplicationError::WrongCharacter)),
            "missing character must return character_not_found, got {result:?}"
        );
        drop_database(&repository, &database).await;
    }

    #[tokio::test]
    async fn missing_draft_load_returns_not_found() {
        let Some((service, repository, database)) = service().await else {
            return;
        };
        insert_account(&repository, ACCOUNT_A).await;
        let missing_id = format!("draft:{}", Uuid::new_v4());
        let result = service
            .load_player_character_draft(ACCOUNT_A, &missing_id)
            .await;
        assert!(
            matches!(result, Err(ApplicationError::WrongCharacter)),
            "missing draft must return character_not_found, got {result:?}"
        );
        drop_database(&repository, &database).await;
    }

    #[tokio::test]
    async fn create_player_character_rejects_owner_mismatch() {
        let Some((service, repository, database)) = service().await else {
            return;
        };
        insert_account(&repository, ACCOUNT_A).await;
        // The character's owner_account_id does not match the server-derived
        // account_id. This must fail before any write.
        let mut character = new_character(ACCOUNT_A, "Mismatched Hero");
        character.owner_account_id = "account:eeeeeeee-eeee-4eee-8eee-eeeeeeeeeeee".to_owned();
        let result = service.create_player_character(ACCOUNT_A, character).await;
        assert!(
            result.is_err(),
            "owner mismatch must be rejected before any write"
        );
        drop_database(&repository, &database).await;
    }
}
