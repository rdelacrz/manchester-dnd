use mongodb::bson::{Bson, Document, doc};

use super::CollectionName;

pub fn validator_for(collection: CollectionName) -> Document {
    use CollectionName::*;

    if collection == SystemSettings {
        return doc! {
            "$jsonSchema": {
                "bsonType": "object",
                "required": ["_id", "schema_version", "revision", "schema_bundle", "updated_at"],
                "additionalProperties": false,
                "properties": {
                    "_id": { "enum": ["system:settings"] },
                    "schema_version": integer_rule(1, 1),
                    "revision": integer_rule(1, i64::MAX),
                    "schema_bundle": {
                        "bsonType": "object",
                        "required": ["version", "digest", "applied_at"],
                        "additionalProperties": false,
                        "properties": {
                            "version": integer_rule(1, i64::MAX),
                            "digest": {
                                "bsonType": "string",
                                "pattern": "^sha256:[0-9a-f]{64}$"
                            },
                            "applied_at": { "bsonType": "date" }
                        }
                    },
                    "updated_at": { "bsonType": "date" }
                }
            }
        };
    }

    let required: &[&str] = match collection {
        Accounts => &[
            "revision",
            "role",
            "username_normalized",
            "email_lookup_hmac",
            "password_phc",
            "login_enabled",
            "created_at",
            "updated_at",
        ],
        SignupAccessTokens => &[
            "token_digest",
            "state",
            "allowed_role",
            "expires_at",
            "purge_at",
        ],
        SignupSessions => &[
            "token_digest",
            "csrf_digest",
            "access_token_id",
            "state",
            "expires_at",
            "purge_at",
        ],
        AccountSessions => &[
            "account_id",
            "bearer_digest",
            "csrf_digest",
            "idle_expires_at",
            "absolute_expires_at",
            "purge_at",
            "created_at",
        ],
        AuthThrottleBuckets => &[
            "key_digest",
            "action_kind",
            "count",
            "window_started_at",
            "purge_at",
        ],
        CampaignInvitations => &["campaign_id", "state", "expires_at", "purge_at"],
        PlayerCharacterDrafts => &[
            "owner_account_id",
            "revision",
            "step",
            "state",
            "updated_at",
            "purge_at",
        ],
        PlayerCharacters => &[
            "owner_account_id",
            "revision",
            "display_name_normalized",
            "created_at",
            "updated_at",
        ],
        Campaigns => &[
            "owner_account_id",
            "revision",
            "title_normalized",
            "members",
            "rules_snapshot",
            "created_at",
            "updated_at",
        ],
        CampaignCharacterInstances => &[
            "campaign_id",
            "account_id",
            "source_player_character_id",
            "revision",
            "state",
            "source_snapshot",
            "progression",
            "runtime",
            "created_at",
            "updated_at",
        ],
        EnemyTemplates => &[
            "logical_id",
            "revision",
            "state",
            "stat_block",
            "created_at",
            "updated_at",
        ],
        CampaignEnemyInstances => &[
            "campaign_id",
            "revision",
            "state",
            "source",
            "runtime",
            "created_at",
            "updated_at",
        ],
        EventTemplates => &[
            "logical_id",
            "revision",
            "state",
            "eligibility",
            "created_at",
        ],
        CampaignEvents => &[
            "campaign_id",
            "revision",
            "status",
            "source_snapshot",
            "created_at",
        ],
        PlaySessions => &[
            "campaign_id",
            "gm_account_id",
            "revision",
            "state",
            "participants",
            "mode",
            "turn_state",
            "opened_at",
            "updated_at",
        ],
        Encounters => &[
            "campaign_id",
            "play_session_id",
            "revision",
            "status",
            "combatants",
            "initiative",
            "created_at",
        ],
        TurnEvents => &[
            "campaign_id",
            "play_session_id",
            "sequence",
            "correlation_id",
            "created_at",
        ],
        CommandReceipts => &[
            "scope_kind",
            "scope_id",
            "actor_account_id",
            "command_kind",
            "idempotency_key",
            "request_fingerprint",
            "state",
            "created_at",
        ],
        AuditEvents => &[
            "category",
            "action",
            "outcome",
            "scope_kind",
            "scope_id",
            "created_at",
        ],
        BdeLedger => &[
            "campaign_character_instance_id",
            "campaign_id",
            "idempotency_key",
            "delta",
            "balance_after",
            "created_at",
        ],
        GenerationJobs => &[
            "campaign_id",
            "purpose",
            "idempotency_key",
            "state",
            "priority",
            "available_at",
            "created_at",
        ],
        GenerationBudgetReservations => &[
            "job_id",
            "scope_kind",
            "scope_id",
            "dimension",
            "state",
            "expires_at",
            "purge_at",
        ],
        GeneratedPresentations => &[
            "campaign_id",
            "origin_event_id",
            "version",
            "selected",
            "created_at",
        ],
        GeneratedAssets => &[
            "owner_account_id",
            "entity_kind",
            "entity_id",
            "object_key",
            "digest",
            "state",
            "created_at",
        ],
        QuarantinedAssets => &["job_id", "reason_code", "purge_at"],
        PrivateInspirationParticipants => &["participant_id", "state", "created_at", "updated_at"],
        PrivateInspirationSources => &[
            "logical_id",
            "revision",
            "review_state",
            "runtime_facts",
            "created_at",
        ],
        PrivateInspirationConsents => &[
            "campaign_id",
            "source_id",
            "participant_id",
            "version",
            "state",
            "created_at",
        ],
        PrivateInspirationVetoes => &[
            "campaign_id",
            "actor_participant_id",
            "scope_kind",
            "state",
            "created_at",
        ],
        PrivateInspirationSelections => &[
            "campaign_id",
            "idempotency_key",
            "turn_number",
            "eligible_set_digest",
            "created_at",
        ],
        PrivateInspirationWork => &[
            "campaign_id",
            "selection_id",
            "state",
            "created_at",
            "updated_at",
        ],
        DeletionPreparations => &[
            "deletion_id",
            "owner_account_id",
            "scope_kind",
            "scope_id",
            "digest",
            "purge_at",
        ],
        DeletionTombstones => &[
            "entity_kind",
            "entity_id",
            "deletion_id",
            "digest",
            "purge_at",
        ],
        SystemSettings => unreachable!("handled above"),
    };

    let mut all_required = vec!["_id", "schema_version"];
    all_required.extend_from_slice(required);
    let mut properties = Document::new();
    for field in &all_required {
        properties.insert(*field, field_rule(field));
    }
    if collection == Accounts {
        properties.insert("role", doc! { "enum": ["admin", "user"] });
    }

    doc! {
        "$jsonSchema": {
            "bsonType": "object",
            "required": all_required,
            "additionalProperties": true,
            "properties": properties
        }
    }
}

fn field_rule(field: &str) -> Bson {
    if field == "_id" {
        return Bson::Document(doc! {
            "bsonType": "string",
            "minLength": 1,
            "maxLength": 128,
            "pattern": "^[A-Za-z0-9._:-]+$"
        });
    }
    if field == "step" {
        // Creation workflows share the draft collection: the player-character
        // library stores a named step while the deterministic hero workflow
        // stores a bounded numeric transition index.
        return Bson::Document(doc! { "bsonType": ["string", "long", "int"] });
    }
    if field == "schema_version" {
        return Bson::Document(integer_rule(1, 1));
    }
    if matches!(
        field,
        "revision"
            | "sequence"
            | "version"
            | "turn_number"
            | "count"
            | "priority"
            | "delta"
            | "balance_after"
    ) {
        return Bson::Document(integer_rule(
            if matches!(field, "delta" | "priority") {
                i64::MIN
            } else {
                0
            },
            i64::MAX,
        ));
    }
    if field.ends_with("_at") {
        return Bson::Document(doc! { "bsonType": "date" });
    }
    if matches!(
        field,
        "members" | "participants" | "combatants" | "themes" | "attempts"
    ) {
        return Bson::Document(doc! { "bsonType": "array", "maxItems": 256 });
    }
    if matches!(
        field,
        "rules_snapshot"
            | "source_snapshot"
            | "source"
            | "progression"
            | "runtime"
            | "stat_block"
            | "eligibility"
            | "turn_state"
            | "initiative"
            | "runtime_facts"
    ) {
        return Bson::Document(doc! { "bsonType": "object" });
    }
    if matches!(field, "login_enabled" | "selected") {
        return Bson::Document(doc! { "bsonType": "bool" });
    }
    Bson::Document(doc! {
        "bsonType": "string",
        "minLength": 1,
        "maxLength": 4096
    })
}

fn integer_rule(minimum: i64, maximum: i64) -> Document {
    doc! {
        "bsonType": ["int", "long"],
        "minimum": minimum,
        "maximum": maximum
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_collection_has_a_real_json_schema_validator() {
        for collection in CollectionName::ALL {
            let validator = validator_for(collection);
            let json_schema = validator
                .get_document("$jsonSchema")
                .expect("validator must use $jsonSchema");
            assert_eq!(json_schema.get_str("bsonType").unwrap(), "object");
            assert!(
                json_schema
                    .get_array("required")
                    .is_ok_and(|required| required.len() >= 2),
                "{}",
                collection.as_str()
            );
        }
    }
}
