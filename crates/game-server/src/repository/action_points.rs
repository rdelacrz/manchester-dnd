//! Campaign-character BDE persistence.
//!
//! MongoDB is authoritative. Balance compare-and-set, ledger, turn evidence,
//! minimized audit, and exact receipt commit in one snapshot/majority
//! transaction. Rich turn mechanics remain in the owning gameplay turn;
//! `bde_ledger` is the append-only point history.

use manchester_dnd_core::{
    Sha256Digest,
    action_points::{
        ActionPointLedgerEntry, ActionPointReason, COST_PER_CUSTOM_ACTION, INITIAL_ACTION_POINTS,
        MAX_ACTION_POINT_BALANCE,
    },
    is_valid_opaque_id,
};
use mongodb::{
    bson::{Bson, DateTime, Document, doc},
    options::ReturnDocument,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::{
    AuditEventDocument, CampaignDocument, CommandReceiptDocument, active_campaign_filter,
    date_string, map_persistence, validate_account_id, validate_opaque,
};
use crate::{
    error::{MongoFailureKind, PersistenceError, RepositoryError},
    persistence::{CollectionName, MongoStore},
};

const SCHEMA_VERSION: u32 = 1;

pub struct ActionPointRepository;

impl ActionPointRepository {
    #[allow(clippy::too_many_arguments)]
    pub async fn grant_initial(
        store: &MongoStore,
        account_id: &str,
        campaign_id: &str,
        runtime_character_id: &str,
        play_session_id: &str,
        turn_revision: u64,
        amount: i32,
        idempotency_key: &str,
    ) -> Result<i32, RepositoryError> {
        Self::apply(
            store,
            account_id,
            campaign_id,
            runtime_character_id,
            play_session_id,
            turn_revision,
            amount,
            ActionPointReason::InitialGrant,
            idempotency_key,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn spend(
        store: &MongoStore,
        account_id: &str,
        campaign_id: &str,
        runtime_character_id: &str,
        play_session_id: &str,
        turn_revision: u64,
        amount: i32,
        idempotency_key: &str,
    ) -> Result<i32, RepositoryError> {
        Self::apply(
            store,
            account_id,
            campaign_id,
            runtime_character_id,
            play_session_id,
            turn_revision,
            amount,
            ActionPointReason::CustomActionSpent,
            idempotency_key,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn refund(
        store: &MongoStore,
        account_id: &str,
        campaign_id: &str,
        runtime_character_id: &str,
        play_session_id: &str,
        turn_revision: u64,
        amount: i32,
        idempotency_key: &str,
    ) -> Result<i32, RepositoryError> {
        Self::apply(
            store,
            account_id,
            campaign_id,
            runtime_character_id,
            play_session_id,
            turn_revision,
            amount,
            ActionPointReason::AdministrativeRefund,
            idempotency_key,
        )
        .await
    }

    pub async fn load_balance(
        store: &MongoStore,
        account_id: &str,
        campaign_id: &str,
        runtime_character_id: &str,
    ) -> Result<i32, RepositoryError> {
        validate_scope(account_id, campaign_id, runtime_character_id, None, None)?;
        let campaigns = store.collection::<CampaignDocument>(CollectionName::Campaigns);
        let instances = store.document_collection(CollectionName::CampaignCharacterInstances);
        let account = account_id.to_owned();
        let campaign = campaign_id.to_owned();
        let character = runtime_character_id.to_owned();
        let stored = store
            .with_transaction(move |client_session| {
                let campaigns = campaigns.clone();
                let instances = instances.clone();
                let account = account.clone();
                let campaign = campaign.clone();
                let character = character.clone();
                Box::pin(async move {
                    campaigns
                        .find_one(active_campaign_filter(&account, &campaign))
                        .session(&mut *client_session)
                        .await
                        .map_err(|error| PersistenceError::mongo("authorize BDE balance", error))?
                        .ok_or_else(|| PersistenceError::NotFound {
                            entity: "campaign",
                            id: campaign.clone(),
                        })?;
                    instances
                        .find_one(doc! {
                            "_id": &character,
                            "campaign_id": &campaign,
                            "account_id": &account,
                            "state": "active",
                        })
                        .session(&mut *client_session)
                        .await
                        .map_err(|error| PersistenceError::mongo("load BDE balance", error))
                })
            })
            .await
            .map_err(map_persistence)?;
        let Some(stored) = stored else {
            return Ok(0);
        };
        optional_i32_path(&stored, "runtime.bde.balance")?.map_or(Ok(0), Ok)
    }

    #[allow(clippy::too_many_arguments)]
    async fn apply(
        store: &MongoStore,
        account_id: &str,
        campaign_id: &str,
        runtime_character_id: &str,
        play_session_id: &str,
        turn_revision: u64,
        amount: i32,
        reason: ActionPointReason,
        idempotency_key: &str,
    ) -> Result<i32, RepositoryError> {
        validate_scope(
            account_id,
            campaign_id,
            runtime_character_id,
            Some(play_session_id),
            Some(idempotency_key),
        )?;
        if amount <= 0 || turn_revision == 0 {
            return invalid(
                runtime_character_id,
                "amount and turn revision must be positive",
            );
        }
        match reason {
            ActionPointReason::InitialGrant if amount != INITIAL_ACTION_POINTS => {
                return invalid(
                    runtime_character_id,
                    "initial BDE grant must use the server policy amount",
                );
            }
            ActionPointReason::CustomActionSpent if amount != COST_PER_CUSTOM_ACTION => {
                return invalid(
                    runtime_character_id,
                    "custom actions must use the server policy cost",
                );
            }
            _ if amount > MAX_ACTION_POINT_BALANCE => {
                return invalid(runtime_character_id, "BDE amount exceeds the server policy");
            }
            _ => {}
        }
        let request = ActionPointRequest {
            account_id,
            campaign_id,
            runtime_character_id,
            play_session_id,
            turn_revision,
            amount,
            reason: reason.as_str(),
        };
        let request_fingerprint = fingerprint(&request)?;
        let delta = reason.delta(amount);
        let turn_revision_i64 =
            i64::try_from(turn_revision).map_err(|_| RepositoryError::NumericRange {
                field: "turn revision",
            })?;
        let result_revision =
            turn_revision
                .checked_add(1)
                .ok_or(RepositoryError::NumericRange {
                    field: "turn revision",
                })?;
        let now = DateTime::now();
        let ledger_id = format!("bde-ledger:{}", Uuid::new_v4().simple());
        let audit_event_id = format!("audit:{}", Uuid::new_v4().simple());
        let receipt_id = format!("receipt:{}", Uuid::new_v4().simple());

        let campaigns = store.collection::<CampaignDocument>(CollectionName::Campaigns);
        let instances = store.document_collection(CollectionName::CampaignCharacterInstances);
        let play_sessions = store.document_collection(CollectionName::PlaySessions);
        let ledgers = store.collection::<BdeLedgerDocument>(CollectionName::BdeLedger);
        let audits = store.collection::<AuditEventDocument>(CollectionName::AuditEvents);
        let receipts = store.collection::<CommandReceiptDocument>(CollectionName::CommandReceipts);
        let account = account_id.to_owned();
        let campaign = campaign_id.to_owned();
        let character = runtime_character_id.to_owned();
        let play_session = play_session_id.to_owned();
        let key = idempotency_key.to_owned();
        let reason_name = reason.as_str().to_owned();
        let fingerprint_text = request_fingerprint.as_str().to_owned();
        let ledger_id_for_write = ledger_id.clone();
        let audit_event_id_for_write = audit_event_id.clone();
        let receipt_id_for_write = receipt_id.clone();
        let transaction = store
            .with_transaction(move |client_session| {
                let campaigns = campaigns.clone();
                let instances = instances.clone();
                let play_sessions = play_sessions.clone();
                let ledgers = ledgers.clone();
                let audits = audits.clone();
                let receipts = receipts.clone();
                let account = account.clone();
                let campaign = campaign.clone();
                let character = character.clone();
                let play_session = play_session.clone();
                let key = key.clone();
                let reason_name = reason_name.clone();
                let fingerprint = fingerprint_text.clone();
                let ledger_id = ledger_id_for_write.clone();
                let audit_event_id = audit_event_id_for_write.clone();
                let receipt_id = receipt_id_for_write.clone();
                Box::pin(async move {
                    campaigns
                        .find_one(active_campaign_filter(&account, &campaign))
                        .session(&mut *client_session)
                        .await
                        .map_err(|error| PersistenceError::mongo("authorize BDE mutation", error))?
                        .ok_or_else(|| PersistenceError::NotFound {
                            entity: "campaign",
                            id: campaign.clone(),
                        })?;
                    play_sessions
                        .find_one(doc! {
                            "_id": &play_session,
                            "campaign_id": &campaign,
                            "state": "active",
                            "$or": [
                                { "gm_account_id": &account },
                                {
                                    "participants": {
                                        "$elemMatch": {
                                            "account_id": &account,
                                            "state": {
                                                "$in": ["active", "human_active", "ai_active"],
                                            },
                                        }
                                    }
                                }
                            ],
                        })
                        .session(&mut *client_session)
                        .await
                        .map_err(|error| {
                            PersistenceError::mongo("authorize BDE play session", error)
                        })?
                        .ok_or_else(|| PersistenceError::NotFound {
                            entity: "active play session",
                            id: play_session.clone(),
                        })?;
                    if let Some(existing) = ledgers
                        .find_one(doc! {
                            "campaign_character_instance_id": &character,
                            "idempotency_key": &key,
                        })
                        .session(&mut *client_session)
                        .await
                        .map_err(|error| PersistenceError::mongo("check BDE replay", error))?
                    {
                        if existing.account_id != account
                            || existing.campaign_id != campaign
                            || existing.play_session_id.as_deref() != Some(play_session.as_str())
                            || existing.turn_revision != turn_revision
                            || existing.amount != amount
                            || existing.delta != delta
                            || existing.reason != reason_name
                            || existing.request_fingerprint != fingerprint
                        {
                            return Err(PersistenceError::IdempotencyConflict {
                                scope_kind: "campaign_character_instance".to_owned(),
                                scope_id: character,
                                idempotency_key: key,
                            });
                        }
                        return Ok(existing.balance_after);
                    }

                    let mut instance_filter = doc! {
                        "_id": &character,
                        "campaign_id": &campaign,
                        "account_id": &account,
                        "state": "active",
                    };
                    let mut increments = doc! {
                        "runtime.bde.balance": delta,
                        "revision": 1_i64,
                    };
                    match reason_name.as_str() {
                        "custom_action_spent" => {
                            instance_filter.insert("runtime.bde.balance", doc! { "$gte": amount });
                            increments.insert("runtime.bde.lifetime_spent", amount);
                        }
                        "administrative_refund" => {
                            instance_filter.insert(
                                "runtime.bde.balance",
                                doc! { "$lte": MAX_ACTION_POINT_BALANCE - amount },
                            );
                            instance_filter
                                .insert("runtime.bde.lifetime_spent", doc! { "$gte": amount });
                            increments.insert("runtime.bde.lifetime_spent", -amount);
                        }
                        _ => {
                            instance_filter.insert(
                                "runtime.bde.balance",
                                doc! { "$lte": MAX_ACTION_POINT_BALANCE - amount },
                            );
                            increments.insert("runtime.bde.lifetime_earned", amount);
                        }
                    }
                    let updated = instances
                        .find_one_and_update(
                            instance_filter,
                            doc! {
                                "$inc": increments,
                                "$set": { "updated_at": now },
                            },
                        )
                        .return_document(ReturnDocument::After)
                        .session(&mut *client_session)
                        .await
                        .map_err(|error| PersistenceError::mongo("apply BDE mutation", error))?
                        .ok_or_else(|| PersistenceError::NotFound {
                            entity: "action point balance",
                            id: character.clone(),
                        })?;
                    let balance = required_i32_path(&updated, "runtime.bde.balance", "BDE balance")
                        .map_err(|detail| PersistenceError::SchemaDrift {
                            collection: CollectionName::CampaignCharacterInstances
                                .as_str()
                                .to_owned(),
                            detail,
                        })?;

                    ledgers
                        .insert_one(BdeLedgerDocument {
                            id: ledger_id.clone(),
                            schema_version: SCHEMA_VERSION,
                            campaign_character_instance_id: character.clone(),
                            account_id: account.clone(),
                            campaign_id: campaign.clone(),
                            play_session_id: Some(play_session.clone()),
                            turn_revision,
                            idempotency_key: key.clone(),
                            request_fingerprint: fingerprint.clone(),
                            amount,
                            delta,
                            reason: reason_name.clone(),
                            balance_after: balance,
                            created_at: now,
                        })
                        .session(&mut *client_session)
                        .await
                        .map_err(|error| PersistenceError::mongo("insert BDE ledger", error))?;
                    audits
                        .insert_one(AuditEventDocument {
                            id: audit_event_id,
                            schema_version: SCHEMA_VERSION,
                            category: "bde".to_owned(),
                            action: reason_name.clone(),
                            outcome: "committed".to_owned(),
                            actor_account_id: Some(account.clone()),
                            scope_kind: "campaign_character_instance".to_owned(),
                            scope_id: character.clone(),
                            correlation_id: Some(ledger_id.clone()),
                            metadata: doc! {
                                "delta": delta,
                                "balance_after": balance,
                                "turn_revision": turn_revision_i64,
                            },
                            created_at: now,
                        })
                        .session(&mut *client_session)
                        .await
                        .map_err(|error| PersistenceError::mongo("insert BDE audit", error))?;
                    receipts
                        .insert_one(CommandReceiptDocument {
                            id: receipt_id,
                            schema_version: SCHEMA_VERSION,
                            scope_kind: "campaign_character_instance".to_owned(),
                            scope_id: character,
                            campaign_id: Some(campaign),
                            actor_account_id: account,
                            command_kind: reason_name,
                            idempotency_key: key,
                            request_fingerprint: fingerprint,
                            state: "committed".to_owned(),
                            expected_revision: turn_revision,
                            result_revision,
                            audit_id: ledger_id,
                            response_json: format!(r#"{{"balance":{balance}}}"#),
                            created_at: now,
                        })
                        .session(&mut *client_session)
                        .await
                        .map_err(|error| PersistenceError::mongo("insert BDE receipt", error))?;
                    Ok(balance)
                })
            })
            .await;
        match transaction {
            Ok(balance) => Ok(balance),
            Err(error) if error.mongo_failure_kind() == Some(MongoFailureKind::DuplicateKey) => {
                replay_after_duplicate(
                    store,
                    account_id,
                    campaign_id,
                    runtime_character_id,
                    play_session_id,
                    idempotency_key,
                    &request_fingerprint,
                )
                .await
            }
            Err(error) => Err(map_persistence(error)),
        }
    }

    pub async fn list_ledger(
        store: &MongoStore,
        account_id: &str,
        campaign_id: &str,
        play_session_id: &str,
    ) -> Result<Vec<ActionPointLedgerEntry>, RepositoryError> {
        validate_scope(
            account_id,
            campaign_id,
            campaign_id,
            Some(play_session_id),
            None,
        )?;
        let campaigns = store.collection::<CampaignDocument>(CollectionName::Campaigns);
        let play_sessions = store.document_collection(CollectionName::PlaySessions);
        let ledgers = store.collection::<BdeLedgerDocument>(CollectionName::BdeLedger);
        let account = account_id.to_owned();
        let campaign = campaign_id.to_owned();
        let play_session = play_session_id.to_owned();
        let documents = store
            .with_transaction(move |client_session| {
                let campaigns = campaigns.clone();
                let play_sessions = play_sessions.clone();
                let ledgers = ledgers.clone();
                let account = account.clone();
                let campaign = campaign.clone();
                let play_session = play_session.clone();
                Box::pin(async move {
                    campaigns
                        .find_one(active_campaign_filter(&account, &campaign))
                        .session(&mut *client_session)
                        .await
                        .map_err(|error| PersistenceError::mongo("authorize BDE ledger", error))?
                        .ok_or_else(|| PersistenceError::NotFound {
                            entity: "campaign",
                            id: campaign.clone(),
                        })?;
                    play_sessions
                        .find_one(doc! {
                            "_id": &play_session,
                            "campaign_id": &campaign,
                            "$or": [
                                { "gm_account_id": &account },
                                { "participants.account_id": &account },
                            ],
                        })
                        .session(&mut *client_session)
                        .await
                        .map_err(|error| {
                            PersistenceError::mongo("authorize BDE ledger play session", error)
                        })?
                        .ok_or_else(|| PersistenceError::NotFound {
                            entity: "play session",
                            id: play_session.clone(),
                        })?;
                    let mut cursor = ledgers
                        .find(doc! {
                            "campaign_id": &campaign,
                            "play_session_id": &play_session,
                        })
                        .sort(doc! { "created_at": 1_i64, "_id": 1_i64 })
                        .session(&mut *client_session)
                        .await
                        .map_err(|error| PersistenceError::mongo("list BDE ledger", error))?;
                    let mut output = Vec::new();
                    while cursor
                        .advance(&mut *client_session)
                        .await
                        .map_err(|error| PersistenceError::mongo("read BDE ledger", error))?
                    {
                        output.push(cursor.deserialize_current().map_err(|error| {
                            PersistenceError::mongo("decode BDE ledger", error)
                        })?);
                    }
                    Ok(output)
                })
            })
            .await
            .map_err(map_persistence)?;
        documents.into_iter().map(domain_ledger_entry).collect()
    }
}

#[derive(Serialize)]
struct ActionPointRequest<'a> {
    account_id: &'a str,
    campaign_id: &'a str,
    runtime_character_id: &'a str,
    play_session_id: &'a str,
    turn_revision: u64,
    amount: i32,
    reason: &'a str,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BdeLedgerDocument {
    #[serde(rename = "_id")]
    id: String,
    schema_version: u32,
    campaign_character_instance_id: String,
    account_id: String,
    campaign_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    play_session_id: Option<String>,
    turn_revision: u64,
    idempotency_key: String,
    request_fingerprint: String,
    amount: i32,
    delta: i32,
    reason: String,
    balance_after: i32,
    created_at: DateTime,
}

fn fingerprint(value: &ActionPointRequest<'_>) -> Result<Sha256Digest, RepositoryError> {
    let bytes = serde_json::to_vec(value).map_err(|source| RepositoryError::Serialize {
        entity: "BDE request fingerprint",
        source,
    })?;
    Ok(Sha256Digest::from_bytes(Sha256::digest(bytes).into()))
}

async fn replay_after_duplicate(
    store: &MongoStore,
    account_id: &str,
    campaign_id: &str,
    character_id: &str,
    play_session_id: &str,
    idempotency_key: &str,
    request_fingerprint: &Sha256Digest,
) -> Result<i32, RepositoryError> {
    let campaigns = store.collection::<CampaignDocument>(CollectionName::Campaigns);
    let play_sessions = store.document_collection(CollectionName::PlaySessions);
    let ledgers = store.collection::<BdeLedgerDocument>(CollectionName::BdeLedger);
    let account = account_id.to_owned();
    let campaign = campaign_id.to_owned();
    let character = character_id.to_owned();
    let play_session = play_session_id.to_owned();
    let key = idempotency_key.to_owned();
    let stored = store
        .with_transaction(move |client_session| {
            let campaigns = campaigns.clone();
            let play_sessions = play_sessions.clone();
            let ledgers = ledgers.clone();
            let account = account.clone();
            let campaign = campaign.clone();
            let character = character.clone();
            let play_session = play_session.clone();
            let key = key.clone();
            Box::pin(async move {
                campaigns
                    .find_one(active_campaign_filter(&account, &campaign))
                    .session(&mut *client_session)
                    .await
                    .map_err(|error| {
                        PersistenceError::mongo("authorize concurrent BDE replay", error)
                    })?
                    .ok_or_else(|| PersistenceError::NotFound {
                        entity: "campaign",
                        id: campaign.clone(),
                    })?;
                play_sessions
                    .find_one(doc! {
                        "_id": &play_session,
                        "campaign_id": &campaign,
                        "state": "active",
                        "$or": [
                            { "gm_account_id": &account },
                            {
                                "participants": {
                                    "$elemMatch": {
                                        "account_id": &account,
                                        "state": {
                                            "$in": ["active", "human_active", "ai_active"],
                                        },
                                    }
                                }
                            }
                        ],
                    })
                    .session(&mut *client_session)
                    .await
                    .map_err(|error| {
                        PersistenceError::mongo("authorize concurrent BDE play session", error)
                    })?
                    .ok_or_else(|| PersistenceError::NotFound {
                        entity: "active play session",
                        id: play_session.clone(),
                    })?;
                ledgers
                    .find_one(doc! {
                        "campaign_character_instance_id": &character,
                        "account_id": &account,
                        "campaign_id": &campaign,
                        "play_session_id": &play_session,
                        "idempotency_key": &key,
                    })
                    .session(&mut *client_session)
                    .await
                    .map_err(|error| PersistenceError::mongo("load concurrent BDE replay", error))
            })
        })
        .await
        .map_err(map_persistence)?
        .ok_or_else(|| RepositoryError::AlreadyExists {
            entity: "BDE command",
            id: idempotency_key.to_owned(),
        })?;
    if stored.request_fingerprint != request_fingerprint.as_str() {
        return Err(RepositoryError::IdempotencyConflict {
            scope_kind: "campaign_character_instance".to_owned(),
            scope_id: character_id.to_owned(),
            idempotency_key: idempotency_key.to_owned(),
        });
    }
    Ok(stored.balance_after)
}

fn domain_ledger_entry(
    stored: BdeLedgerDocument,
) -> Result<ActionPointLedgerEntry, RepositoryError> {
    let reason = ActionPointReason::parse(&stored.reason).ok_or_else(|| {
        RepositoryError::InvalidDomainState {
            entity: "BDE ledger",
            id: stored.id.clone(),
            reason: "stored BDE reason is unsupported",
        }
    })?;
    Ok(ActionPointLedgerEntry {
        account_id: stored.account_id,
        campaign_id: stored.campaign_id,
        runtime_character_id: stored.campaign_character_instance_id,
        play_session_id: stored.play_session_id.unwrap_or_default(),
        turn_revision: stored.turn_revision,
        amount: stored.amount,
        reason,
        idempotency_key: stored.idempotency_key,
        created_at: date_string(stored.created_at),
    })
}

fn optional_i32_path(document: &Document, path: &str) -> Result<Option<i32>, RepositoryError> {
    let mut current = document;
    let mut components = path.split('.').peekable();
    while let Some(component) = components.next() {
        let value = match current.get(component) {
            Some(value) => value,
            None => return Ok(None),
        };
        if components.peek().is_none() {
            return integer_value(value)
                .map(|number| {
                    i32::try_from(number).map_err(|_| RepositoryError::NumericRange {
                        field: "BDE balance",
                    })
                })
                .transpose();
        }
        current = value
            .as_document()
            .ok_or_else(|| RepositoryError::InvalidDomainState {
                entity: "campaign character instance",
                id: "runtime".to_owned(),
                reason: "stored BDE runtime has an invalid shape",
            })?;
    }
    Ok(None)
}

fn required_i32_path(document: &Document, path: &str, field: &str) -> Result<i32, String> {
    let mut current = document;
    let mut components = path.split('.').peekable();
    while let Some(component) = components.next() {
        let value = current
            .get(component)
            .ok_or_else(|| format!("{field} is missing"))?;
        if components.peek().is_none() {
            let value = integer_value(value).ok_or_else(|| format!("{field} is not an integer"))?;
            return i32::try_from(value).map_err(|_| format!("{field} is outside i32"));
        }
        current = value
            .as_document()
            .ok_or_else(|| format!("{field} parent is not a document"))?;
    }
    Err(format!("{field} is missing"))
}

fn integer_value(value: &Bson) -> Option<i64> {
    match value {
        Bson::Int32(value) => Some(i64::from(*value)),
        Bson::Int64(value) => Some(*value),
        _ => None,
    }
}

fn validate_scope(
    account_id: &str,
    campaign_id: &str,
    runtime_character_id: &str,
    play_session_id: Option<&str>,
    idempotency_key: Option<&str>,
) -> Result<(), RepositoryError> {
    validate_account_id(account_id)?;
    validate_opaque("campaign", campaign_id)?;
    validate_opaque("campaign character instance", runtime_character_id)?;
    if play_session_id.is_some_and(|id| !is_valid_opaque_id(id))
        || idempotency_key.is_some_and(|id| !is_valid_opaque_id(id))
    {
        return invalid(
            runtime_character_id,
            "play-session and idempotency identifiers must be valid",
        );
    }
    Ok(())
}

fn invalid<T>(id: &str, reason: &'static str) -> Result<T, RepositoryError> {
    Err(RepositoryError::InvalidDomainState {
        entity: "BDE",
        id: id.to_owned(),
        reason,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_fingerprint_changes_with_amount_or_reason() {
        let request = ActionPointRequest {
            account_id: "account:test",
            campaign_id: "campaign:test",
            runtime_character_id: "campaign-character:test",
            play_session_id: "play-session:test",
            turn_revision: 4,
            amount: 1,
            reason: "custom_action_spent",
        };
        let first = fingerprint(&request).unwrap();
        let changed = ActionPointRequest {
            amount: 2,
            ..request
        };
        assert_ne!(first, fingerprint(&changed).unwrap());
    }
}
