//! MongoDB persistence for account-owned, level-less player characters.

use manchester_dnd_core::{
    PlayerCharacter,
    hero::{HeroChoices, HeroError},
    is_valid_opaque_id,
};
use mongodb::{
    Collection,
    bson::{DateTime, doc},
};
use serde::{Deserialize, Serialize};

use super::MongoRepository;
use crate::{
    error::{MongoFailureKind, PersistenceError, RepositoryError},
    persistence::CollectionName,
};

const DRAFT_RETENTION_SECONDS: i64 = 30 * 24 * 60 * 60;

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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PlayerCharacterDocument {
    #[serde(rename = "_id")]
    pub(crate) id: String,
    pub(crate) schema_version: i64,
    pub(crate) revision: i64,
    pub(crate) owner_account_id: String,
    pub(crate) display_name: String,
    pub(crate) display_name_normalized: String,
    pub(crate) ruleset_id: String,
    pub(crate) build_blueprint: HeroChoices,
    pub(crate) created_at: DateTime,
    pub(crate) updated_at: DateTime,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PlayerCharacterDraftDocument {
    #[serde(rename = "_id")]
    id: String,
    schema_version: i64,
    revision: i64,
    owner_account_id: String,
    step: String,
    state: String,
    choices: Option<HeroChoices>,
    committed_character_id: Option<String>,
    expires_at: DateTime,
    purge_at: DateTime,
    created_at: DateTime,
    updated_at: DateTime,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PlayerCharacterReceiptDocument {
    #[serde(rename = "_id")]
    id: String,
    schema_version: i64,
    scope_kind: String,
    scope_id: String,
    actor_account_id: String,
    command_kind: String,
    idempotency_key: String,
    request_fingerprint: String,
    result_revision: i64,
    response_json: serde_json::Value,
    state: String,
    created_at: DateTime,
}

impl MongoRepository {
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
        require_account(self, account_id).await?;

        let now = DateTime::now();
        let stored = PlayerCharacterDocument {
            id: character.character_id.clone(),
            schema_version: i64::from(character.schema_version),
            revision: revision_to_i64(character.revision)?,
            owner_account_id: account_id.to_owned(),
            display_name: character.display_name.clone(),
            display_name_normalized: normalize_display_name(&character.display_name),
            ruleset_id: character.choices.pins.ruleset_id.as_str().to_owned(),
            build_blueprint: character.choices.clone(),
            created_at: now,
            updated_at: now,
        };
        self.player_characters()
            .insert_one(stored)
            .await
            .map_err(|error| {
                map_mongo_write(
                    error,
                    "create player character",
                    "player_character",
                    &character.character_id,
                )
            })?;
        Ok(character.clone())
    }

    pub async fn load_player_character(
        &self,
        account_id: &str,
        character_id: &str,
    ) -> Result<Option<PlayerCharacter>, RepositoryError> {
        validate_account_id(account_id)?;
        validate_character_id(character_id)?;
        self.player_characters()
            .find_one(doc! {
                "_id": character_id,
                "owner_account_id": account_id,
            })
            .await
            .map_err(|error| mongo_error("load player character", error))?
            .map(player_character_from_document)
            .transpose()
    }

    pub async fn list_player_characters(
        &self,
        account_id: &str,
    ) -> Result<Vec<PlayerCharacterSummary>, RepositoryError> {
        validate_account_id(account_id)?;
        let mut cursor = self
            .player_characters()
            .find(doc! { "owner_account_id": account_id })
            .sort(doc! { "updated_at": -1, "_id": 1 })
            .await
            .map_err(|error| mongo_error("list player characters", error))?;
        let mut output = Vec::new();
        while cursor
            .advance()
            .await
            .map_err(|error| mongo_error("read player character list", error))?
        {
            let stored = cursor
                .deserialize_current()
                .map_err(|error| mongo_error("decode player character list", error))?;
            output.push(player_character_summary(stored)?);
        }
        Ok(output)
    }

    pub async fn update_player_character_display_name(
        &self,
        account_id: &str,
        character_id: &str,
        expected_revision: u64,
        new_display_name: &str,
    ) -> Result<u64, RepositoryError> {
        validate_account_id(account_id)?;
        validate_character_id(character_id)?;
        validate_display_name(new_display_name)?;
        let expected = revision_to_i64(expected_revision)?;
        let next = expected_revision
            .checked_add(1)
            .ok_or(RepositoryError::NumericRange { field: "revision" })?;
        let result = self
            .player_characters()
            .update_one(
                doc! {
                    "_id": character_id,
                    "owner_account_id": account_id,
                    "revision": expected,
                },
                doc! {
                    "$set": {
                        "display_name": new_display_name,
                        "display_name_normalized": normalize_display_name(new_display_name),
                        "updated_at": DateTime::now(),
                    },
                    "$inc": { "revision": 1_i64 },
                },
            )
            .await
            .map_err(|error| {
                map_mongo_write(
                    error,
                    "update player character display name",
                    "player_character",
                    character_id,
                )
            })?;
        if result.modified_count == 1 {
            return Ok(next);
        }
        character_write_miss(self, account_id, character_id, expected_revision).await
    }

    pub async fn delete_player_character(
        &self,
        account_id: &str,
        character_id: &str,
    ) -> Result<bool, RepositoryError> {
        validate_account_id(account_id)?;
        validate_character_id(character_id)?;
        let characters = self.player_characters();
        let instances = self
            .store()
            .document_collection(CollectionName::CampaignCharacterInstances);
        let account_id = account_id.to_owned();
        let character_id = character_id.to_owned();
        self.with_transaction(move |session| {
            let characters = characters.clone();
            let instances = instances.clone();
            let account_id = account_id.clone();
            let character_id = character_id.clone();
            Box::pin(async move {
                let owned = characters
                    .find_one(doc! {
                        "_id": &character_id,
                        "owner_account_id": &account_id,
                    })
                    .session(&mut *session)
                    .await
                    .map_err(|error| {
                        PersistenceError::mongo("load character before delete", error)
                    })?;
                if owned.is_none() {
                    return Ok(false);
                }
                let active_reference = instances
                    .find_one(doc! {
                        "account_id": &account_id,
                        "source_player_character_id": &character_id,
                        "state": "active",
                    })
                    .session(&mut *session)
                    .await
                    .map_err(|error| {
                        PersistenceError::mongo("check active character instances", error)
                    })?;
                if active_reference.is_some() {
                    return Err(PersistenceError::AlreadyExists {
                        entity: "active campaign character instance",
                        id: character_id,
                    });
                }
                let deleted = characters
                    .delete_one(doc! {
                        "_id": &character_id,
                        "owner_account_id": &account_id,
                    })
                    .session(&mut *session)
                    .await
                    .map_err(|error| PersistenceError::mongo("delete player character", error))?;
                Ok(deleted.deleted_count == 1)
            })
        })
        .await
        .map_err(map_persistence_error)
    }

    pub async fn create_player_character_draft(
        &self,
        account_id: &str,
        draft_id: &str,
        expires_at_epoch_seconds: u64,
    ) -> Result<PlayerCharacterDraftSummary, RepositoryError> {
        validate_account_id(account_id)?;
        validate_draft_id(draft_id)?;
        require_account(self, account_id).await?;
        let expires_at = epoch_seconds_to_date(expires_at_epoch_seconds)?;
        let purge_at = DateTime::from_millis(
            expires_at
                .timestamp_millis()
                .saturating_add(DRAFT_RETENTION_SECONDS.saturating_mul(1_000)),
        );
        let now = DateTime::now();
        let stored = PlayerCharacterDraftDocument {
            id: draft_id.to_owned(),
            schema_version: 1,
            revision: 0,
            owner_account_id: account_id.to_owned(),
            step: "campaign_theme".to_owned(),
            state: "editing".to_owned(),
            choices: None,
            committed_character_id: None,
            expires_at,
            purge_at,
            created_at: now,
            updated_at: now,
        };
        self.player_character_drafts()
            .insert_one(stored.clone())
            .await
            .map_err(|error| {
                map_mongo_write(
                    error,
                    "create player character draft",
                    "player_character_draft",
                    draft_id,
                )
            })?;
        draft_summary(stored)
    }

    pub async fn load_player_character_draft(
        &self,
        account_id: &str,
        draft_id: &str,
    ) -> Result<Option<(PlayerCharacterDraftSummary, Option<HeroChoices>)>, RepositoryError> {
        validate_account_id(account_id)?;
        validate_draft_id(draft_id)?;
        self.player_character_drafts()
            .find_one(doc! {
                "_id": draft_id,
                "owner_account_id": account_id,
            })
            .await
            .map_err(|error| mongo_error("load player character draft", error))?
            .map(|stored| {
                let choices = stored.choices.clone();
                Ok((draft_summary(stored)?, choices))
            })
            .transpose()
    }

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
        validate_step(step)?;
        choices
            .validate()
            .map_err(|error| to_repository_error("player_character_draft", draft_id, &error))?;
        let expected = revision_to_i64(expected_revision)?;
        let choices = mongodb::bson::to_bson(choices)?;
        let result = self
            .player_character_drafts()
            .update_one(
                doc! {
                    "_id": draft_id,
                    "owner_account_id": account_id,
                    "revision": expected,
                    "state": "editing",
                },
                doc! {
                    "$set": {
                        "choices": choices,
                        "step": step,
                        "updated_at": DateTime::now(),
                    },
                    "$inc": { "revision": 1_i64 },
                },
            )
            .await
            .map_err(|error| mongo_error("save player character draft", error))?;
        if result.modified_count == 1 {
            return expected_revision
                .checked_add(1)
                .ok_or(RepositoryError::NumericRange { field: "revision" });
        }
        draft_write_miss(self, account_id, draft_id, expected_revision).await
    }

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
        let expected = revision_to_i64(expected_revision)?;
        let drafts = self.player_character_drafts();
        let characters = self.player_characters();
        let account_id = account_id.to_owned();
        let draft_id = draft_id.to_owned();
        let character_id = character_id.to_owned();
        let account_id_for_miss = account_id.clone();
        let draft_id_for_miss = draft_id.clone();
        let result = self
            .with_transaction(move |session| {
                let drafts = drafts.clone();
                let characters = characters.clone();
                let account_id = account_id.clone();
                let draft_id = draft_id.clone();
                let character_id = character_id.clone();
                Box::pin(async move {
                    let character = characters
                        .find_one(doc! {
                            "_id": &character_id,
                            "owner_account_id": &account_id,
                        })
                        .session(&mut *session)
                        .await
                        .map_err(|error| {
                            PersistenceError::mongo("load committed character", error)
                        })?;
                    if character.is_none() {
                        return Err(PersistenceError::NotFound {
                            entity: "player_character",
                            id: character_id,
                        });
                    }
                    let update = drafts
                        .update_one(
                            doc! {
                                "_id": &draft_id,
                                "owner_account_id": &account_id,
                                "revision": expected,
                                "state": "editing",
                            },
                            doc! {
                                "$set": {
                                    "state": "committed",
                                    "committed_character_id": &character_id,
                                    "updated_at": DateTime::now(),
                                },
                                "$inc": { "revision": 1_i64 },
                            },
                        )
                        .session(&mut *session)
                        .await
                        .map_err(|error| {
                            PersistenceError::mongo("commit player character draft", error)
                        })?;
                    if update.modified_count != 1 {
                        return Err(PersistenceError::RevisionConflict {
                            entity: "player_character_draft",
                            id: draft_id,
                            expected: expected_revision,
                            actual: expected_revision.saturating_add(1),
                        });
                    }
                    Ok(())
                })
            })
            .await;
        match result {
            Ok(()) => expected_revision
                .checked_add(1)
                .ok_or(RepositoryError::NumericRange { field: "revision" }),
            Err(PersistenceError::RevisionConflict { .. }) => {
                draft_write_miss(
                    self,
                    account_id_for_miss.as_str(),
                    draft_id_for_miss.as_str(),
                    expected_revision,
                )
                .await
            }
            Err(error) => Err(map_persistence_error(error)),
        }
    }

    pub async fn delete_player_character_draft(
        &self,
        account_id: &str,
        draft_id: &str,
    ) -> Result<bool, RepositoryError> {
        validate_account_id(account_id)?;
        validate_draft_id(draft_id)?;
        let deleted = self
            .player_character_drafts()
            .delete_one(doc! {
                "_id": draft_id,
                "owner_account_id": account_id,
            })
            .await
            .map_err(|error| mongo_error("delete player character draft", error))?;
        Ok(deleted.deleted_count == 1)
    }

    pub async fn cleanup_expired_player_character_drafts(&self) -> Result<u64, RepositoryError> {
        let deleted = self
            .player_character_drafts()
            .delete_many(doc! { "purge_at": { "$lte": DateTime::now() } })
            .await
            .map_err(|error| mongo_error("clean expired player character drafts", error))?;
        Ok(deleted.deleted_count)
    }

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
        let audit_bson = mongodb::bson::to_bson(&audit_json)?;
        let revision = revision_to_i64(revision)?;
        let characters = self.player_characters();
        let audits = self
            .store()
            .document_collection(CollectionName::AuditEvents);
        let account_id = account_id.to_owned();
        let character_id = character_id.to_owned();
        let action = action.to_owned();
        self.with_transaction(move |session| {
            let characters = characters.clone();
            let audits = audits.clone();
            let account_id = account_id.clone();
            let character_id = character_id.clone();
            let action = action.clone();
            let audit_bson = audit_bson.clone();
            Box::pin(async move {
                let owned = characters
                    .find_one(doc! {
                        "_id": &character_id,
                        "owner_account_id": &account_id,
                    })
                    .session(&mut *session)
                    .await
                    .map_err(|error| {
                        PersistenceError::mongo("authorize player character audit", error)
                    })?;
                if owned.is_none() {
                    return Err(PersistenceError::NotFound {
                        entity: "player_character",
                        id: character_id,
                    });
                }
                audits
                    .insert_one(doc! {
                        "_id": format!("audit:{}", uuid::Uuid::new_v4()),
                        "schema_version": 1_i64,
                        "category": "player_character",
                        "action": action,
                        "outcome": "committed",
                        "scope_kind": "player_character",
                        "scope_id": character_id,
                        "actor_account_id": account_id,
                        "revision": revision,
                        "metadata": audit_bson,
                        "created_at": DateTime::now(),
                    })
                    .session(&mut *session)
                    .await
                    .map_err(|error| {
                        PersistenceError::mongo("insert player character audit", error)
                    })?;
                Ok(())
            })
        })
        .await
        .map_err(map_persistence_error)?;
        Ok(())
    }

    pub async fn load_player_character_command_receipt(
        &self,
        account_id: &str,
        character_id: &str,
        idempotency_key: &str,
    ) -> Result<Option<StoredPlayerCharacterReceipt>, RepositoryError> {
        validate_account_id(account_id)?;
        validate_receipt_entity_id(character_id)?;
        validate_idempotency_key(idempotency_key)?;
        let scope_kind = receipt_scope_kind(character_id);
        self.player_character_receipts()
            .find_one(doc! {
                "scope_kind": scope_kind,
                "scope_id": character_id,
                "actor_account_id": account_id,
                "idempotency_key": idempotency_key,
                "state": "committed",
            })
            .await
            .map_err(|error| mongo_error("load player character receipt", error))?
            .map(stored_receipt)
            .transpose()
    }

    pub async fn insert_player_character_command_receipt(
        &self,
        receipt: &NewPlayerCharacterReceipt<'_>,
    ) -> Result<(), RepositoryError> {
        validate_account_id(receipt.owner_account_id)?;
        validate_receipt_entity_id(receipt.character_id)?;
        validate_idempotency_key(receipt.idempotency_key)?;
        validate_command_kind(receipt.command_kind)?;
        validate_fingerprint(&receipt.request_fingerprint)?;
        let stored = PlayerCharacterReceiptDocument {
            id: format!("receipt:{}", uuid::Uuid::new_v4()),
            schema_version: 1,
            scope_kind: receipt_scope_kind(receipt.character_id).to_owned(),
            scope_id: receipt.character_id.to_owned(),
            actor_account_id: receipt.owner_account_id.to_owned(),
            command_kind: receipt.command_kind.to_owned(),
            idempotency_key: receipt.idempotency_key.to_owned(),
            request_fingerprint: receipt.request_fingerprint.clone(),
            result_revision: revision_to_i64(receipt.result_revision)?,
            response_json: receipt.response_json.clone(),
            state: "committed".to_owned(),
            created_at: DateTime::now(),
        };
        let subjects = if receipt.character_id.starts_with("draft:") {
            self.store()
                .document_collection(CollectionName::PlayerCharacterDrafts)
        } else {
            self.store()
                .document_collection(CollectionName::PlayerCharacters)
        };
        let receipts = self.player_character_receipts();
        let owner_account_id = receipt.owner_account_id.to_owned();
        let subject_id = receipt.character_id.to_owned();
        let result = self
            .with_transaction(move |session| {
                let subjects = subjects.clone();
                let receipts = receipts.clone();
                let owner_account_id = owner_account_id.clone();
                let subject_id = subject_id.clone();
                let stored = stored.clone();
                Box::pin(async move {
                    let owned = subjects
                        .find_one(doc! {
                            "_id": &subject_id,
                            "owner_account_id": &owner_account_id,
                        })
                        .projection(doc! { "_id": 1 })
                        .session(&mut *session)
                        .await
                        .map_err(|error| {
                            PersistenceError::mongo("authorize player character receipt", error)
                        })?;
                    if owned.is_none() {
                        return Err(PersistenceError::NotFound {
                            entity: "player_character_receipt_subject",
                            id: subject_id,
                        });
                    }
                    receipts
                        .insert_one(stored)
                        .session(&mut *session)
                        .await
                        .map_err(|error| {
                            PersistenceError::mongo("insert player character receipt", error)
                        })?;
                    Ok(())
                })
            })
            .await;
        if let Err(error) = result {
            return Err(match error.mongo_failure_kind() {
                Some(MongoFailureKind::DuplicateKey) => RepositoryError::AlreadyExists {
                    entity: "player_character_command_receipt",
                    id: receipt.character_id.to_owned(),
                },
                Some(MongoFailureKind::DocumentValidation) => RepositoryError::InvalidDomainState {
                    entity: "player_character_command_receipt",
                    id: receipt.character_id.to_owned(),
                    reason: "document failed MongoDB schema validation",
                },
                _ => map_persistence_error(error),
            });
        }
        Ok(())
    }

    fn player_characters(&self) -> Collection<PlayerCharacterDocument> {
        self.store().collection(CollectionName::PlayerCharacters)
    }

    fn player_character_drafts(&self) -> Collection<PlayerCharacterDraftDocument> {
        self.store()
            .collection(CollectionName::PlayerCharacterDrafts)
    }

    fn player_character_receipts(&self) -> Collection<PlayerCharacterReceiptDocument> {
        self.store().collection(CollectionName::CommandReceipts)
    }
}

async fn require_account(
    repository: &MongoRepository,
    account_id: &str,
) -> Result<(), RepositoryError> {
    let account = repository
        .store()
        .document_collection(CollectionName::Accounts)
        .find_one(doc! { "_id": account_id })
        .projection(doc! { "_id": 1 })
        .await
        .map_err(|error| mongo_error("load character owner account", error))?;
    if account.is_none() {
        return Err(RepositoryError::NotFound {
            entity: "account",
            id: account_id.to_owned(),
        });
    }
    Ok(())
}

async fn character_write_miss(
    repository: &MongoRepository,
    account_id: &str,
    character_id: &str,
    expected_revision: u64,
) -> Result<u64, RepositoryError> {
    let current = repository
        .player_characters()
        .find_one(doc! {
            "_id": character_id,
            "owner_account_id": account_id,
        })
        .await
        .map_err(|error| mongo_error("resolve player character write", error))?;
    match current {
        Some(stored) => Err(RepositoryError::RevisionConflict {
            entity: "player_character",
            id: character_id.to_owned(),
            expected: expected_revision,
            actual: revision_from_i64(stored.revision, "revision")?,
        }),
        None => Err(RepositoryError::NotFound {
            entity: "player_character",
            id: character_id.to_owned(),
        }),
    }
}

async fn draft_write_miss(
    repository: &MongoRepository,
    account_id: &str,
    draft_id: &str,
    expected_revision: u64,
) -> Result<u64, RepositoryError> {
    let current = repository
        .player_character_drafts()
        .find_one(doc! {
            "_id": draft_id,
            "owner_account_id": account_id,
        })
        .await
        .map_err(|error| mongo_error("resolve player character draft write", error))?;
    match current {
        Some(stored) => Err(RepositoryError::RevisionConflict {
            entity: "player_character_draft",
            id: draft_id.to_owned(),
            expected: expected_revision,
            actual: revision_from_i64(stored.revision, "revision")?,
        }),
        None => Err(RepositoryError::NotFound {
            entity: "player_character_draft",
            id: draft_id.to_owned(),
        }),
    }
}

pub(crate) fn player_character_from_document(
    stored: PlayerCharacterDocument,
) -> Result<PlayerCharacter, RepositoryError> {
    if stored.ruleset_id != stored.build_blueprint.pins.ruleset_id.as_str()
        || stored.display_name_normalized != normalize_display_name(&stored.display_name)
    {
        return invalid(
            "player_character",
            &stored.id,
            "stored library envelope is inconsistent",
        );
    }
    let character = PlayerCharacter {
        schema_version: u16::try_from(stored.schema_version).map_err(|_| {
            RepositoryError::NumericRange {
                field: "player character schema version",
            }
        })?,
        character_id: stored.id.clone(),
        owner_account_id: stored.owner_account_id,
        revision: revision_from_i64(stored.revision, "revision")?,
        display_name: stored.display_name,
        choices: stored.build_blueprint,
    };
    character.validate().map_err(|error| {
        to_repository_error("player_character", &character.character_id, &error)
    })?;
    Ok(character)
}

fn player_character_summary(
    stored: PlayerCharacterDocument,
) -> Result<PlayerCharacterSummary, RepositoryError> {
    Ok(PlayerCharacterSummary {
        id: stored.id,
        owner_account_id: stored.owner_account_id,
        revision: revision_from_i64(stored.revision, "revision")?,
        display_name: stored.display_name,
        created_at: date_string(stored.created_at, "player_characters")?,
        updated_at: date_string(stored.updated_at, "player_characters")?,
    })
}

fn draft_summary(
    stored: PlayerCharacterDraftDocument,
) -> Result<PlayerCharacterDraftSummary, RepositoryError> {
    Ok(PlayerCharacterDraftSummary {
        id: stored.id,
        owner_account_id: stored.owner_account_id,
        revision: revision_from_i64(stored.revision, "revision")?,
        expires_at: date_string(stored.expires_at, "player_character_drafts")?,
        step: stored.step,
        reviewed: stored.state == "committed",
        committed_character_id: stored.committed_character_id,
        created_at: date_string(stored.created_at, "player_character_drafts")?,
        updated_at: date_string(stored.updated_at, "player_character_drafts")?,
    })
}

fn stored_receipt(
    stored: PlayerCharacterReceiptDocument,
) -> Result<StoredPlayerCharacterReceipt, RepositoryError> {
    Ok(StoredPlayerCharacterReceipt {
        character_id: stored.scope_id,
        owner_account_id: stored.actor_account_id,
        idempotency_key: stored.idempotency_key,
        command_kind: stored.command_kind,
        request_fingerprint: stored.request_fingerprint,
        result_revision: revision_from_i64(stored.result_revision, "result_revision")?,
        response_json: stored.response_json,
        created_at: date_string(stored.created_at, "command_receipts")?,
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

fn validate_receipt_entity_id(entity_id: &str) -> Result<(), RepositoryError> {
    if (entity_id.starts_with("character:") || entity_id.starts_with("draft:"))
        && is_valid_opaque_id(entity_id)
    {
        return Ok(());
    }
    invalid(
        "player_character_command_receipt",
        entity_id,
        "entity identifier is invalid",
    )
}

fn validate_display_name(name: &str) -> Result<(), RepositoryError> {
    if name.trim().is_empty() || name.chars().count() > 200 || name.chars().any(char::is_control) {
        return invalid(
            "player_character",
            "display-name",
            "display name must be 1-200 non-control characters",
        );
    }
    Ok(())
}

fn validate_step(step: &str) -> Result<(), RepositoryError> {
    if step.is_empty()
        || step.len() > 64
        || !step
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return invalid(
            "player_character_draft",
            "step",
            "draft step must be a bounded opaque label",
        );
    }
    Ok(())
}

fn validate_idempotency_key(key: &str) -> Result<(), RepositoryError> {
    if key.is_empty()
        || key.len() > 128
        || !key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
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
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
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

fn normalize_display_name(name: &str) -> String {
    name.trim().to_lowercase()
}

fn receipt_scope_kind(id: &str) -> &'static str {
    if id.starts_with("draft:") {
        "player_character_draft"
    } else {
        "player_character"
    }
}

fn to_repository_error(entity: &'static str, id: &str, _error: &HeroError) -> RepositoryError {
    RepositoryError::InvalidDomainState {
        entity,
        id: id.to_owned(),
        reason: "failed hero-domain validation",
    }
}

fn revision_to_i64(value: u64) -> Result<i64, RepositoryError> {
    i64::try_from(value).map_err(|_| RepositoryError::NumericRange { field: "revision" })
}

fn revision_from_i64(value: i64, field: &'static str) -> Result<u64, RepositoryError> {
    u64::try_from(value).map_err(|_| RepositoryError::NumericRange { field })
}

fn epoch_seconds_to_date(value: u64) -> Result<DateTime, RepositoryError> {
    let milliseconds = value
        .checked_mul(1_000)
        .and_then(|value| i64::try_from(value).ok())
        .ok_or(RepositoryError::NumericRange {
            field: "expires_at",
        })?;
    Ok(DateTime::from_millis(milliseconds))
}

fn date_string(value: DateTime, collection: &str) -> Result<String, RepositoryError> {
    value.try_to_rfc3339_string().map_err(|_| {
        RepositoryError::Persistence(PersistenceError::SchemaDrift {
            collection: collection.to_owned(),
            detail: "stored BSON date is outside RFC 3339 range".to_owned(),
        })
    })
}

fn mongo_error(operation: &'static str, error: mongodb::error::Error) -> RepositoryError {
    RepositoryError::Persistence(PersistenceError::mongo(operation, error))
}

fn map_mongo_write(
    error: mongodb::error::Error,
    operation: &'static str,
    entity: &'static str,
    id: &str,
) -> RepositoryError {
    let persistence = PersistenceError::mongo(operation, error);
    match persistence.mongo_failure_kind() {
        Some(MongoFailureKind::DuplicateKey) => RepositoryError::AlreadyExists {
            entity,
            id: id.to_owned(),
        },
        Some(MongoFailureKind::DocumentValidation) => RepositoryError::InvalidDomainState {
            entity,
            id: id.to_owned(),
            reason: "document failed MongoDB schema validation",
        },
        _ => RepositoryError::Persistence(persistence),
    }
}

fn map_persistence_error(error: PersistenceError) -> RepositoryError {
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

    use manchester_dnd_core::hero::{
        AncestryId, BackgroundId, BackgroundSelection, ClassSelection, EquipmentId,
        EquipmentSelection, FightingStyleId, HeroChoices, HeroConceptId, HeroPins,
        HeroPresentation, SkillId, StandardArrayAssignment, ThemeId,
    };
    use mongodb::bson::doc;
    use uuid::Uuid;

    use super::*;
    use crate::{
        config::{MongoConfig, MongoSchemaPolicy, SecretString},
        persistence::SchemaReconciler,
    };

    async fn test_repository() -> Option<(MongoRepository, String)> {
        let Ok(uri) = std::env::var("MONGODB_TEST_URI") else {
            eprintln!("skipping MongoDB contract: MONGODB_TEST_URI is not set");
            return None;
        };
        assert!(!uri.trim().is_empty());
        let database = format!("mdnd_test_characters_{}", Uuid::new_v4().simple());
        let store = crate::persistence::MongoStore::connect(&MongoConfig {
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

    fn choices() -> HeroChoices {
        HeroChoices {
            pins: HeroPins::mvp(ThemeId::RainboundBorough),
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
                appearance: "Weathered".to_owned(),
                ideal: "Justice".to_owned(),
                bond: "The canal".to_owned(),
                flaw: "Too trusting".to_owned(),
                tone_limits: Vec::new(),
            },
        }
    }

    #[tokio::test]
    async fn mongo_character_contract_covers_isolation_revision_drafts_and_level_boundary() {
        let Some((repository, database)) = test_repository().await else {
            return;
        };
        let account_a = format!("account:{}", Uuid::new_v4());
        let account_b = format!("account:{}", Uuid::new_v4());
        insert_account(&repository, &account_a).await;
        insert_account(&repository, &account_b).await;
        let character_id = format!("character:{}", Uuid::new_v4());
        let character = PlayerCharacter::new(
            character_id.clone(),
            account_a.clone(),
            "Mara".to_owned(),
            choices(),
        )
        .expect("character fixture must validate");

        repository
            .create_player_character(&account_a, &character)
            .await
            .expect("create must work");
        assert!(
            repository
                .load_player_character(&account_b, &character_id)
                .await
                .expect("foreign load must be safe")
                .is_none()
        );
        let raw = repository
            .store()
            .document_collection(CollectionName::PlayerCharacters)
            .find_one(doc! { "_id": &character_id })
            .await
            .expect("raw read must work")
            .expect("character must exist");
        for forbidden in [
            "level",
            "experience_points",
            "current_hit_points",
            "maximum_hit_points",
            "campaign_id",
            "runtime",
            "progression",
        ] {
            assert!(!raw.contains_key(forbidden), "{forbidden}");
        }

        assert_eq!(
            repository
                .update_player_character_display_name(&account_a, &character_id, 0, "Mara Venn")
                .await
                .expect("CAS update must work"),
            1
        );
        assert!(matches!(
            repository
                .update_player_character_display_name(&account_a, &character_id, 0, "Stale")
                .await,
            Err(RepositoryError::RevisionConflict { .. })
        ));

        let draft_id = format!("draft:{}", Uuid::new_v4());
        repository
            .create_player_character_draft(&account_a, &draft_id, 4_102_444_800)
            .await
            .expect("draft create must work");
        repository
            .save_player_character_draft_choices(&account_a, &draft_id, 0, &choices(), "review")
            .await
            .expect("draft save must work");
        assert!(matches!(
            repository
                .save_player_character_draft_choices(&account_a, &draft_id, 0, &choices(), "stale",)
                .await,
            Err(RepositoryError::RevisionConflict { .. })
        ));
        repository
            .commit_player_character_draft(&account_a, &draft_id, 1, &character_id)
            .await
            .expect("draft commit must work");
        let (draft, loaded_choices) = repository
            .load_player_character_draft(&account_a, &draft_id)
            .await
            .expect("draft load must work")
            .expect("draft must exist");
        assert!(draft.reviewed);
        assert!(loaded_choices.is_some());
        assert!(
            repository
                .load_player_character_draft(&account_b, &draft_id)
                .await
                .expect("foreign draft load must be safe")
                .is_none()
        );

        let fingerprint = format!("sha256:{}", "a".repeat(64));
        let foreign_receipt = NewPlayerCharacterReceipt {
            owner_account_id: &account_b,
            character_id: &character_id,
            idempotency_key: "foreign-receipt",
            command_kind: "player_character_update_display_name",
            request_fingerprint: fingerprint.clone(),
            result_revision: 1,
            response_json: serde_json::json!({ "result_revision": 1 }),
        };
        assert!(matches!(
            repository
                .insert_player_character_command_receipt(&foreign_receipt)
                .await,
            Err(RepositoryError::NotFound { .. })
        ));
        let receipt = NewPlayerCharacterReceipt {
            owner_account_id: &account_a,
            character_id: &character_id,
            idempotency_key: "rename-once",
            command_kind: "player_character_update_display_name",
            request_fingerprint: fingerprint.clone(),
            result_revision: 1,
            response_json: serde_json::json!({ "result_revision": 1 }),
        };
        repository
            .insert_player_character_command_receipt(&receipt)
            .await
            .expect("receipt insert must work");
        assert_eq!(
            repository
                .load_player_character_command_receipt(&account_a, &character_id, "rename-once")
                .await
                .expect("receipt load must work")
                .expect("receipt must exist")
                .request_fingerprint,
            fingerprint
        );
        assert!(
            repository
                .load_player_character_command_receipt(&account_b, &character_id, "rename-once")
                .await
                .expect("foreign receipt lookup must be safe")
                .is_none()
        );

        assert!(
            database.starts_with("mdnd_test_characters_"),
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
