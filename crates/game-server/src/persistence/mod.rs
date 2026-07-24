pub(crate) mod auth;
pub(crate) mod email_crypto;
mod indexes;
mod mongo;
mod schema;
mod transaction;
mod validators;

pub use indexes::IndexSpec;
pub use mongo::MongoStore;
pub use schema::{
    SCHEMA_BUNDLE_VERSION, SchemaApplyReport, SchemaCatalogEntry, SchemaReconciler,
    SchemaVerificationReport, collection_catalog, schema_bundle_digest,
};
pub use transaction::TransactionFuture;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CollectionName {
    Accounts,
    SignupAccessTokens,
    SignupSessions,
    AccountSessions,
    AuthThrottleBuckets,
    CampaignInvitations,
    PlayerCharacterDrafts,
    PlayerCharacters,
    Campaigns,
    CampaignCharacterInstances,
    EnemyTemplates,
    CampaignEnemyInstances,
    EventTemplates,
    CampaignEvents,
    PlaySessions,
    Encounters,
    TurnEvents,
    CommandReceipts,
    AuditEvents,
    BdeLedger,
    GenerationJobs,
    GenerationBudgetReservations,
    GeneratedPresentations,
    GeneratedAssets,
    QuarantinedAssets,
    PrivateInspirationParticipants,
    PrivateInspirationSources,
    PrivateInspirationConsents,
    PrivateInspirationVetoes,
    PrivateInspirationSelections,
    PrivateInspirationWork,
    DeletionPreparations,
    DeletionTombstones,
    SystemSettings,
}

impl CollectionName {
    pub const ALL: [Self; 34] = [
        Self::Accounts,
        Self::SignupAccessTokens,
        Self::SignupSessions,
        Self::AccountSessions,
        Self::AuthThrottleBuckets,
        Self::CampaignInvitations,
        Self::PlayerCharacterDrafts,
        Self::PlayerCharacters,
        Self::Campaigns,
        Self::CampaignCharacterInstances,
        Self::EnemyTemplates,
        Self::CampaignEnemyInstances,
        Self::EventTemplates,
        Self::CampaignEvents,
        Self::PlaySessions,
        Self::Encounters,
        Self::TurnEvents,
        Self::CommandReceipts,
        Self::AuditEvents,
        Self::BdeLedger,
        Self::GenerationJobs,
        Self::GenerationBudgetReservations,
        Self::GeneratedPresentations,
        Self::GeneratedAssets,
        Self::QuarantinedAssets,
        Self::PrivateInspirationParticipants,
        Self::PrivateInspirationSources,
        Self::PrivateInspirationConsents,
        Self::PrivateInspirationVetoes,
        Self::PrivateInspirationSelections,
        Self::PrivateInspirationWork,
        Self::DeletionPreparations,
        Self::DeletionTombstones,
        Self::SystemSettings,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Accounts => "accounts",
            Self::SignupAccessTokens => "signup_access_tokens",
            Self::SignupSessions => "signup_sessions",
            Self::AccountSessions => "account_sessions",
            Self::AuthThrottleBuckets => "auth_throttle_buckets",
            Self::CampaignInvitations => "campaign_invitations",
            Self::PlayerCharacterDrafts => "player_character_drafts",
            Self::PlayerCharacters => "player_characters",
            Self::Campaigns => "campaigns",
            Self::CampaignCharacterInstances => "campaign_character_instances",
            Self::EnemyTemplates => "enemy_templates",
            Self::CampaignEnemyInstances => "campaign_enemy_instances",
            Self::EventTemplates => "event_templates",
            Self::CampaignEvents => "campaign_events",
            Self::PlaySessions => "play_sessions",
            Self::Encounters => "encounters",
            Self::TurnEvents => "turn_events",
            Self::CommandReceipts => "command_receipts",
            Self::AuditEvents => "audit_events",
            Self::BdeLedger => "bde_ledger",
            Self::GenerationJobs => "generation_jobs",
            Self::GenerationBudgetReservations => "generation_budget_reservations",
            Self::GeneratedPresentations => "generated_presentations",
            Self::GeneratedAssets => "generated_assets",
            Self::QuarantinedAssets => "quarantined_assets",
            Self::PrivateInspirationParticipants => "private_inspiration_participants",
            Self::PrivateInspirationSources => "private_inspiration_sources",
            Self::PrivateInspirationConsents => "private_inspiration_consents",
            Self::PrivateInspirationVetoes => "private_inspiration_vetoes",
            Self::PrivateInspirationSelections => "private_inspiration_selections",
            Self::PrivateInspirationWork => "private_inspiration_work",
            Self::DeletionPreparations => "deletion_preparations",
            Self::DeletionTombstones => "deletion_tombstones",
            Self::SystemSettings => "system_settings",
        }
    }
}

pub use auth::MongoAccountRepository;
pub use email_crypto::{EmailCiphertext, EmailCrypto, EmailCryptoError};

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn complete_target_collection_catalog_is_unique() {
        assert_eq!(CollectionName::ALL.len(), 34);
        let unique = CollectionName::ALL
            .iter()
            .map(|name| name.as_str())
            .collect::<HashSet<_>>();
        assert_eq!(unique.len(), 34);
    }
}
