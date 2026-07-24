use mongodb::bson::{Document, doc};

use super::CollectionName;

pub const MANAGED_INDEX_PREFIX: &str = "mdnd_";

#[derive(Debug, Clone, PartialEq)]
pub struct IndexSpec {
    pub name: &'static str,
    pub keys: Document,
    pub unique: bool,
    pub partial_filter: Option<Document>,
    pub expire_after_seconds: Option<i64>,
}

impl IndexSpec {
    pub fn command_document(&self) -> Document {
        let mut document = doc! {
            "name": self.name,
            "key": self.keys.clone(),
        };
        if self.unique {
            document.insert("unique", true);
        }
        if let Some(filter) = &self.partial_filter {
            document.insert("partialFilterExpression", filter.clone());
        }
        if let Some(seconds) = self.expire_after_seconds {
            document.insert("expireAfterSeconds", seconds);
        }
        document
    }
}

fn index(name: &'static str, keys: Document) -> IndexSpec {
    IndexSpec {
        name,
        keys,
        unique: false,
        partial_filter: None,
        expire_after_seconds: None,
    }
}

fn unique(name: &'static str, keys: Document) -> IndexSpec {
    IndexSpec {
        unique: true,
        ..index(name, keys)
    }
}

fn partial_unique(name: &'static str, keys: Document, filter: Document) -> IndexSpec {
    IndexSpec {
        partial_filter: Some(filter),
        ..unique(name, keys)
    }
}

fn ttl(name: &'static str) -> IndexSpec {
    IndexSpec {
        expire_after_seconds: Some(0),
        ..index(name, doc! { "purge_at": 1 })
    }
}

pub fn indexes_for(collection: CollectionName) -> Vec<IndexSpec> {
    use CollectionName::*;

    match collection {
        Accounts => vec![
            unique(
                "mdnd_accounts_username_normalized_uq",
                doc! { "username_normalized": 1 },
            ),
            unique(
                "mdnd_accounts_email_lookup_hmac_uq",
                doc! { "email_lookup_hmac": 1 },
            ),
            index(
                "mdnd_accounts_role_created",
                doc! { "role": 1, "created_at": 1 },
            ),
        ],
        SignupAccessTokens => vec![
            unique(
                "mdnd_signup_access_tokens_digest_uq",
                doc! { "token_digest": 1 },
            ),
            index(
                "mdnd_signup_access_tokens_state_expiry",
                doc! { "state": 1, "expires_at": 1 },
            ),
            ttl("mdnd_signup_access_tokens_purge_ttl"),
        ],
        SignupSessions => vec![
            unique(
                "mdnd_signup_sessions_token_digest_uq",
                doc! { "token_digest": 1 },
            ),
            unique(
                "mdnd_signup_sessions_csrf_digest_uq",
                doc! { "csrf_digest": 1 },
            ),
            partial_unique(
                "mdnd_signup_sessions_active_access_token_uq",
                doc! { "access_token_id": 1 },
                doc! { "state": "active" },
            ),
            ttl("mdnd_signup_sessions_purge_ttl"),
        ],
        AccountSessions => vec![
            unique(
                "mdnd_account_sessions_bearer_digest_uq",
                doc! { "bearer_digest": 1 },
            ),
            unique(
                "mdnd_account_sessions_csrf_digest_uq",
                doc! { "csrf_digest": 1 },
            ),
            index(
                "mdnd_account_sessions_account_revoked_created",
                doc! { "account_id": 1, "revoked_at": 1, "created_at": 1 },
            ),
            ttl("mdnd_account_sessions_purge_ttl"),
        ],
        AuthThrottleBuckets => vec![
            unique(
                "mdnd_auth_throttle_key_action_uq",
                doc! { "key_digest": 1, "action_kind": 1 },
            ),
            ttl("mdnd_auth_throttle_purge_ttl"),
        ],
        CampaignInvitations => vec![
            partial_unique(
                "mdnd_campaign_invitations_active_code_uq",
                doc! { "join_code_digest": 1 },
                doc! { "state": "active", "join_code_digest": { "$type": "string" } },
            ),
            partial_unique(
                "mdnd_campaign_invitations_active_email_uq",
                doc! { "campaign_id": 1, "invitee_email_lookup_hmac": 1 },
                doc! {
                    "state": "active",
                    "invitee_email_lookup_hmac": { "$type": "string" }
                },
            ),
            index(
                "mdnd_campaign_invitations_campaign_state_expiry",
                doc! { "campaign_id": 1, "state": 1, "expires_at": 1 },
            ),
            ttl("mdnd_campaign_invitations_purge_ttl"),
        ],
        PlayerCharacterDrafts => vec![
            index(
                "mdnd_player_character_drafts_owner_updated",
                doc! { "owner_account_id": 1, "updated_at": -1 },
            ),
            ttl("mdnd_player_character_drafts_purge_ttl"),
        ],
        PlayerCharacters => vec![
            unique(
                "mdnd_player_characters_owner_name_uq",
                doc! { "owner_account_id": 1, "display_name_normalized": 1 },
            ),
            index(
                "mdnd_player_characters_owner_updated",
                doc! { "owner_account_id": 1, "updated_at": -1 },
            ),
        ],
        Campaigns => vec![
            index(
                "mdnd_campaigns_owner_updated",
                doc! { "owner_account_id": 1, "updated_at": -1 },
            ),
            index(
                "mdnd_campaigns_member_state_updated",
                doc! { "members.account_id": 1, "members.state": 1, "updated_at": -1 },
            ),
            unique(
                "mdnd_campaigns_owner_title_uq",
                doc! { "owner_account_id": 1, "title_normalized": 1 },
            ),
        ],
        CampaignCharacterInstances => vec![
            partial_unique(
                "mdnd_campaign_character_instances_active_account_uq",
                doc! { "campaign_id": 1, "account_id": 1 },
                doc! { "state": "active" },
            ),
            index(
                "mdnd_campaign_character_instances_source_campaign",
                doc! {
                    "account_id": 1,
                    "source_player_character_id": 1,
                    "campaign_id": 1
                },
            ),
            index(
                "mdnd_campaign_character_instances_campaign_state",
                doc! { "campaign_id": 1, "state": 1 },
            ),
        ],
        EnemyTemplates => vec![
            unique(
                "mdnd_enemy_templates_logical_revision_uq",
                doc! { "logical_id": 1, "revision": 1 },
            ),
            partial_unique(
                "mdnd_enemy_templates_current_logical_uq",
                doc! { "logical_id": 1 },
                doc! { "state": "current" },
            ),
            index(
                "mdnd_enemy_templates_state_updated",
                doc! { "state": 1, "updated_at": -1 },
            ),
        ],
        CampaignEnemyInstances => vec![
            index(
                "mdnd_campaign_enemy_instances_campaign_state_updated",
                doc! { "campaign_id": 1, "state": 1, "updated_at": -1 },
            ),
            index(
                "mdnd_campaign_enemy_instances_source",
                doc! { "source.logical_id": 1, "source.revision": 1 },
            ),
        ],
        EventTemplates => vec![
            unique(
                "mdnd_event_templates_logical_revision_uq",
                doc! { "logical_id": 1, "revision": 1 },
            ),
            partial_unique(
                "mdnd_event_templates_current_logical_uq",
                doc! { "logical_id": 1 },
                doc! { "state": "current" },
            ),
            index(
                "mdnd_event_templates_state_eligibility",
                doc! { "state": 1, "eligibility.mode": 1 },
            ),
        ],
        CampaignEvents => vec![
            index(
                "mdnd_campaign_events_campaign_created",
                doc! { "campaign_id": 1, "created_at": -1 },
            ),
            index(
                "mdnd_campaign_events_session_turn",
                doc! { "play_session_id": 1, "turn_sequence": 1 },
            ),
            index(
                "mdnd_campaign_events_campaign_status",
                doc! { "campaign_id": 1, "status": 1 },
            ),
        ],
        PlaySessions => vec![
            partial_unique(
                "mdnd_play_sessions_open_campaign_uq",
                doc! { "campaign_id": 1 },
                doc! { "state": { "$in": ["waiting", "active"] } },
            ),
            index(
                "mdnd_play_sessions_participant_state",
                doc! { "participants.account_id": 1, "state": 1 },
            ),
            index(
                "mdnd_play_sessions_campaign_opened",
                doc! { "campaign_id": 1, "opened_at": -1 },
            ),
        ],
        Encounters => vec![
            partial_unique(
                "mdnd_encounters_active_session_uq",
                doc! { "play_session_id": 1 },
                doc! { "status": "active" },
            ),
            index(
                "mdnd_encounters_campaign_created",
                doc! { "campaign_id": 1, "created_at": -1 },
            ),
        ],
        TurnEvents => vec![
            unique(
                "mdnd_turn_events_session_sequence_uq",
                doc! { "play_session_id": 1, "sequence": 1 },
            ),
            index(
                "mdnd_turn_events_campaign_created",
                doc! { "campaign_id": 1, "created_at": -1 },
            ),
            index("mdnd_turn_events_correlation", doc! { "correlation_id": 1 }),
        ],
        CommandReceipts => vec![
            unique(
                "mdnd_command_receipts_scope_key_uq",
                doc! { "scope_kind": 1, "scope_id": 1, "idempotency_key": 1 },
            ),
            index(
                "mdnd_command_receipts_actor_created",
                doc! { "actor_account_id": 1, "created_at": -1 },
            ),
            ttl("mdnd_command_receipts_purge_ttl"),
        ],
        AuditEvents => vec![
            index(
                "mdnd_audit_events_scope_created",
                doc! { "scope_kind": 1, "scope_id": 1, "created_at": -1 },
            ),
            index(
                "mdnd_audit_events_actor_created",
                doc! { "actor_account_id": 1, "created_at": -1 },
            ),
            index(
                "mdnd_audit_events_category_created",
                doc! { "category": 1, "created_at": -1 },
            ),
            ttl("mdnd_audit_events_purge_ttl"),
        ],
        BdeLedger => vec![
            unique(
                "mdnd_bde_ledger_instance_key_uq",
                doc! { "campaign_character_instance_id": 1, "idempotency_key": 1 },
            ),
            index(
                "mdnd_bde_ledger_session_created",
                doc! { "play_session_id": 1, "created_at": -1 },
            ),
        ],
        GenerationJobs => vec![
            unique(
                "mdnd_generation_jobs_campaign_purpose_key_uq",
                doc! { "campaign_id": 1, "purpose": 1, "idempotency_key": 1 },
            ),
            index(
                "mdnd_generation_jobs_claim",
                doc! { "state": 1, "available_at": 1, "priority": -1 },
            ),
            index(
                "mdnd_generation_jobs_lease_expiry",
                doc! { "lease_expires_at": 1 },
            ),
        ],
        GenerationBudgetReservations => vec![
            unique(
                "mdnd_generation_budget_job_dimension_uq",
                doc! { "job_id": 1, "dimension": 1 },
            ),
            index(
                "mdnd_generation_budget_scope_state_expiry",
                doc! { "scope_kind": 1, "scope_id": 1, "state": 1, "expires_at": 1 },
            ),
            ttl("mdnd_generation_budget_purge_ttl"),
        ],
        GeneratedPresentations => vec![
            unique(
                "mdnd_generated_presentations_origin_version_uq",
                doc! { "campaign_id": 1, "origin_event_id": 1, "version": 1 },
            ),
            partial_unique(
                "mdnd_generated_presentations_selected_origin_uq",
                doc! { "campaign_id": 1, "origin_event_id": 1 },
                doc! { "selected": true },
            ),
            index(
                "mdnd_generated_presentations_campaign_created",
                doc! { "campaign_id": 1, "created_at": -1 },
            ),
        ],
        GeneratedAssets => vec![
            index(
                "mdnd_generated_assets_owner_entity",
                doc! { "owner_account_id": 1, "entity_kind": 1, "entity_id": 1 },
            ),
            index(
                "mdnd_generated_assets_campaign_created",
                doc! { "campaign_id": 1, "created_at": -1 },
            ),
            unique(
                "mdnd_generated_assets_object_key_uq",
                doc! { "object_key": 1 },
            ),
        ],
        QuarantinedAssets => vec![
            ttl("mdnd_quarantined_assets_purge_ttl"),
            index("mdnd_quarantined_assets_job", doc! { "job_id": 1 }),
        ],
        PrivateInspirationParticipants => vec![
            unique(
                "mdnd_private_participants_id_uq",
                doc! { "participant_id": 1 },
            ),
            index(
                "mdnd_private_participants_state_time",
                doc! { "state": 1, "updated_at": -1 },
            ),
        ],
        PrivateInspirationSources => vec![
            unique(
                "mdnd_private_sources_logical_revision_uq",
                doc! { "logical_id": 1, "revision": 1 },
            ),
            index(
                "mdnd_private_sources_review_theme_expiry",
                doc! { "review_state": 1, "themes": 1, "expires_at": 1 },
            ),
        ],
        PrivateInspirationConsents => vec![
            unique(
                "mdnd_private_consents_versioned_grant_uq",
                doc! {
                    "campaign_id": 1,
                    "source_id": 1,
                    "participant_id": 1,
                    "version": 1
                },
            ),
            index(
                "mdnd_private_consents_campaign_source_state",
                doc! { "campaign_id": 1, "source_id": 1, "state": 1 },
            ),
        ],
        PrivateInspirationVetoes => vec![index(
            "mdnd_private_vetoes_campaign_scope",
            doc! {
                "campaign_id": 1,
                "state": 1,
                "scope_kind": 1,
                "category_id": 1,
                "source_id": 1
            },
        )],
        PrivateInspirationSelections => vec![
            unique(
                "mdnd_private_selections_campaign_key_uq",
                doc! { "campaign_id": 1, "idempotency_key": 1 },
            ),
            index(
                "mdnd_private_selections_source_turn",
                doc! { "campaign_id": 1, "source_id": 1, "turn_number": -1 },
            ),
        ],
        PrivateInspirationWork => vec![
            unique("mdnd_private_work_selection_uq", doc! { "selection_id": 1 }),
            index(
                "mdnd_private_work_campaign_state",
                doc! { "campaign_id": 1, "state": 1 },
            ),
        ],
        DeletionPreparations => vec![
            unique(
                "mdnd_deletion_preparations_id_uq",
                doc! { "deletion_id": 1 },
            ),
            ttl("mdnd_deletion_preparations_purge_ttl"),
        ],
        DeletionTombstones => vec![
            unique(
                "mdnd_deletion_tombstones_entity_deletion_uq",
                doc! { "entity_kind": 1, "entity_id": 1, "deletion_id": 1 },
            ),
            ttl("mdnd_deletion_tombstones_purge_ttl"),
        ],
        SystemSettings => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_ttl_index_is_single_field_and_immediate() {
        for collection in CollectionName::ALL {
            for index in indexes_for(collection) {
                if index.expire_after_seconds.is_some() {
                    assert_eq!(index.keys, doc! { "purge_at": 1 }, "{}", index.name);
                    assert_eq!(index.expire_after_seconds, Some(0), "{}", index.name);
                }
            }
        }
    }

    #[test]
    fn every_managed_index_has_a_unique_stable_name() {
        let mut names = std::collections::HashSet::new();
        for collection in CollectionName::ALL {
            for index in indexes_for(collection) {
                assert!(index.name.starts_with(MANAGED_INDEX_PREFIX));
                assert!(
                    names.insert(index.name),
                    "duplicate index name {}",
                    index.name
                );
            }
        }
    }
}
