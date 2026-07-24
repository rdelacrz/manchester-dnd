//! MongoDB campaign membership, invitation, and character-instance storage.

#![allow(dead_code)]

use manchester_dnd_core::{
    PlayerCharacter, SESSION_SCHEMA_VERSION, SessionDto, SessionStatus, hero::HeroCharacter,
    is_valid_opaque_id,
};
use mongodb::{
    Collection,
    bson::{DateTime, doc},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::{
    MongoRepository,
    player_characters::{PlayerCharacterDocument, player_character_from_document},
};
use crate::{
    error::{MongoFailureKind, PersistenceError, RepositoryError},
    persistence::CollectionName,
};

pub const MEMBERSHIP_SCHEMA_VERSION: u16 = 1;
pub const INVITATION_TTL_SECONDS: u64 = 7 * 24 * 60 * 60;

const MAX_CAMPAIGN_TITLE_LEN: usize = 200;
const MAX_EMAIL_LEN: usize = 320;
const MAX_CAMPAIGN_MEMBERS: usize = 16;
const INVITATION_RETENTION_SECONDS: i64 = 30 * 24 * 60 * 60;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MembershipCampaignSummary {
    pub campaign_id: String,
    pub title: String,
    pub theme_id: String,
    pub role: MembershipRole,
    pub state: MembershipState,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CampaignMembershipRow {
    pub campaign_id: String,
    pub account_id: String,
    pub role: MembershipRole,
    pub state: MembershipState,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CampaignInvitationRow {
    pub id: String,
    pub campaign_id: String,
    pub inviter_account_id: String,
    pub invitee_email_digest: String,
    pub expires_at: String,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CampaignCharacterInstanceRow {
    pub campaign_id: String,
    pub account_id: String,
    pub instance_id: String,
    pub source_player_character_id: String,
    pub runtime_hero_character_id: String,
    pub source_display_name: String,
    pub source_choices_digest: String,
    pub state: CharacterInstanceState,
    pub created_at: String,
    pub retired_at: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MembershipRole {
    GameMaster,
    Player,
}

impl MembershipRole {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::GameMaster => "game_master",
            Self::Player => "player",
        }
    }

    fn try_from_str(value: &str) -> Result<Self, RepositoryError> {
        match value {
            "game_master" => Ok(Self::GameMaster),
            "player" => Ok(Self::Player),
            _ => invalid("campaign_membership", value, "unknown membership role"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MembershipState {
    Invited,
    Active,
    Left,
    Removed,
}

impl MembershipState {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Invited => "invited",
            Self::Active => "active",
            Self::Left => "left",
            Self::Removed => "removed",
        }
    }

    fn try_from_str(value: &str) -> Result<Self, RepositoryError> {
        match value {
            "invited" => Ok(Self::Invited),
            "active" => Ok(Self::Active),
            "left" => Ok(Self::Left),
            "removed" => Ok(Self::Removed),
            _ => invalid("campaign_membership", value, "unknown membership state"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CharacterInstanceState {
    Active,
    Retired,
}

impl CharacterInstanceState {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Retired => "retired",
        }
    }

    fn try_from_str(value: &str) -> Result<Self, RepositoryError> {
        match value {
            "active" => Ok(Self::Active),
            "retired" => Ok(Self::Retired),
            _ => invalid(
                "campaign_character_instance",
                value,
                "unknown instance state",
            ),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateCampaignWithOwnerOutcome {
    pub campaign_id: String,
    pub title: String,
    pub theme_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssignCharacterOutcome {
    pub instance_id: String,
    pub runtime_hero_character_id: String,
}

use super::{CampaignDocument, CampaignLifecycleDocument, CampaignMemberDocument};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CampaignInvitationDocument {
    #[serde(rename = "_id")]
    id: String,
    schema_version: i64,
    campaign_id: String,
    inviter_account_id: String,
    invitee_email_lookup_hmac: String,
    state: String,
    expires_at: DateTime,
    purge_at: DateTime,
    accepted_account_id: Option<String>,
    accepted_at: Option<DateTime>,
    revoked_at: Option<DateTime>,
    created_at: DateTime,
    updated_at: DateTime,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SourceCharacterSnapshot {
    pub(crate) source_revision: i64,
    pub(crate) source_schema_version: i64,
    pub(crate) source_digest: String,
    pub(crate) captured_at: DateTime,
    pub(crate) display_name: String,
    pub(crate) player_character: PlayerCharacter,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CampaignProgressionDocument {
    pub(crate) level: i64,
    pub(crate) experience_points: i64,
    pub(crate) milestone_count: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CampaignRuntimeDocument {
    pub(crate) hero: HeroCharacter,
    pub(crate) current_hit_points: i64,
    pub(crate) maximum_hit_points: i64,
    pub(crate) temporary_hit_points: i64,
    pub(crate) bde_balance: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CampaignCharacterInstanceDocument {
    #[serde(rename = "_id")]
    pub(crate) id: String,
    pub(crate) schema_version: i64,
    pub(crate) revision: i64,
    pub(crate) campaign_id: String,
    pub(crate) account_id: String,
    pub(crate) source_player_character_id: String,
    pub(crate) runtime_hero_character_id: String,
    pub(crate) state: String,
    pub(crate) source_snapshot: SourceCharacterSnapshot,
    pub(crate) progression: CampaignProgressionDocument,
    pub(crate) runtime: CampaignRuntimeDocument,
    pub(crate) created_at: DateTime,
    pub(crate) updated_at: DateTime,
    pub(crate) retired_at: Option<DateTime>,
}

impl MongoRepository {
    pub async fn create_campaign_with_owner(
        &self,
        account_id: &str,
        title: &str,
        theme_id: &str,
    ) -> Result<CreateCampaignWithOwnerOutcome, RepositoryError> {
        validate_account_id(account_id)?;
        validate_campaign_title(title)?;
        validate_theme_id(theme_id)?;
        require_account(self, account_id).await?;
        if let Some(existing) = self
            .campaigns()
            .find_one(doc! {
                "owner_account_id": account_id,
                "title_normalized": normalize_title(title),
            })
            .await
            .map_err(|error| mongo_error("load campaign create replay", error))?
        {
            if existing.theme_id == theme_id {
                return Ok(CreateCampaignWithOwnerOutcome {
                    campaign_id: existing.id,
                    title: existing.title,
                    theme_id: existing.theme_id,
                });
            }
            return Err(RepositoryError::AlreadyExists {
                entity: "campaign title",
                id: title.to_owned(),
            });
        }

        let campaign_id = format!("campaign:{}", Uuid::new_v4());
        let now = DateTime::now();
        let now_ms =
            u64::try_from(now.timestamp_millis()).map_err(|_| RepositoryError::NumericRange {
                field: "campaign timestamp",
            })?;
        let session = SessionDto {
            schema_version: SESSION_SCHEMA_VERSION,
            id: campaign_id.clone(),
            ruleset: manchester_dnd_core::RulesetId::Srd5_1,
            title: title.to_owned(),
            status: SessionStatus::Active,
            character_ids: Vec::new(),
            created_at_unix_ms: now_ms,
            updated_at_unix_ms: now_ms,
            last_event_sequence: 0,
        };
        session
            .validate()
            .map_err(|source| RepositoryError::CoreValidation {
                entity: "campaign session",
                id: campaign_id.clone(),
                source,
            })?;
        let campaign = CampaignDocument {
            id: campaign_id.clone(),
            schema_version: 1,
            revision: 1,
            gameplay_revision: 1,
            lifecycle_revision: 1,
            owner_account_id: account_id.to_owned(),
            title: title.to_owned(),
            title_normalized: normalize_title(title),
            theme_id: theme_id.to_owned(),
            lifecycle: CampaignLifecycleDocument {
                state: "open".to_owned(),
                archived_at: None,
            },
            members: vec![CampaignMemberDocument {
                account_id: account_id.to_owned(),
                role: MembershipRole::GameMaster.as_str().to_owned(),
                state: MembershipState::Active.as_str().to_owned(),
                inviter_account_id: Some(account_id.to_owned()),
                joined_at: now,
                left_at: None,
                created_at: now,
                updated_at: now,
            }],
            rules_snapshot: doc! {
                "state": "unsealed",
                "ruleset_id": "srd-5.1",
                "theme_id": theme_id,
            },
            safety_policy_id: "safety:private-v1".to_owned(),
            progression_policy_id: "progression:xp-v1".to_owned(),
            retention_class: "campaign_lifetime".to_owned(),
            retention_delete_after: None,
            current_play_session_id: None,
            session,
            created_at: now,
            updated_at: now,
        };
        let campaigns = self.campaigns();
        let audits = self
            .store()
            .document_collection(CollectionName::AuditEvents);
        let audit_id = format!("audit:{}", Uuid::new_v4());
        let account_id_owned = account_id.to_owned();
        let campaign_id_owned = campaign_id.clone();
        let result = self
            .with_transaction(move |client_session| {
                let campaigns = campaigns.clone();
                let audits = audits.clone();
                let campaign = campaign.clone();
                let audit_id = audit_id.clone();
                let account_id = account_id_owned.clone();
                let campaign_id = campaign_id_owned.clone();
                Box::pin(async move {
                    campaigns
                        .insert_one(campaign)
                        .session(&mut *client_session)
                        .await
                        .map_err(|error| PersistenceError::mongo("create owned campaign", error))?;
                    audits
                        .insert_one(doc! {
                            "_id": audit_id,
                            "schema_version": 1_i64,
                            "category": "campaign_lifecycle",
                            "action": "campaign_created",
                            "outcome": "committed",
                            "scope_kind": "campaign",
                            "scope_id": campaign_id,
                            "actor_account_id": account_id,
                            "revision": 1_i64,
                            "metadata": {},
                            "created_at": DateTime::now(),
                        })
                        .session(&mut *client_session)
                        .await
                        .map_err(|error| {
                            PersistenceError::mongo("audit owned campaign creation", error)
                        })?;
                    Ok(())
                })
            })
            .await;
        if let Err(error) = result {
            if error.mongo_failure_kind() == Some(MongoFailureKind::DuplicateKey)
                && let Some(existing) = self
                    .campaigns()
                    .find_one(doc! {
                        "owner_account_id": account_id,
                        "title_normalized": normalize_title(title),
                        "theme_id": theme_id,
                    })
                    .await
                    .map_err(|source| mongo_error("resolve campaign create replay", source))?
            {
                return Ok(CreateCampaignWithOwnerOutcome {
                    campaign_id: existing.id,
                    title: existing.title,
                    theme_id: existing.theme_id,
                });
            }
            return Err(map_transaction_error(
                error,
                "campaign session",
                &campaign_id,
            ));
        }
        Ok(CreateCampaignWithOwnerOutcome {
            campaign_id,
            title: title.to_owned(),
            theme_id: theme_id.to_owned(),
        })
    }

    pub async fn create_campaign_invitation(
        &self,
        gm_account_id: &str,
        campaign_id: &str,
        invitee_email: &str,
        expires_at_unix_seconds: u64,
    ) -> Result<CampaignInvitationRow, RepositoryError> {
        validate_account_id(gm_account_id)?;
        validate_campaign_id(campaign_id)?;
        validate_email(invitee_email)?;
        let expires_at = epoch_seconds_to_date(expires_at_unix_seconds, "invitation expiry")?;
        let now = DateTime::now();
        let maximum_expiry = DateTime::from_millis(
            now.timestamp_millis().saturating_add(
                i64::try_from(INVITATION_TTL_SECONDS)
                    .map_err(|_| RepositoryError::NumericRange {
                        field: "invitation TTL",
                    })?
                    .saturating_mul(1_000),
            ),
        );
        if expires_at <= now || expires_at > maximum_expiry {
            return invalid(
                "campaign_invitation",
                campaign_id,
                "invitation expiry must be in the future and within the configured TTL",
            );
        }
        let purge_at = DateTime::from_millis(
            expires_at
                .timestamp_millis()
                .saturating_add(INVITATION_RETENTION_SECONDS.saturating_mul(1_000)),
        );
        let invitation = CampaignInvitationDocument {
            id: format!("invitation:{}", Uuid::new_v4()),
            schema_version: 1,
            campaign_id: campaign_id.to_owned(),
            inviter_account_id: gm_account_id.to_owned(),
            invitee_email_lookup_hmac: invitation_email_digest(campaign_id, invitee_email),
            state: "active".to_owned(),
            expires_at,
            purge_at,
            accepted_account_id: None,
            accepted_at: None,
            revoked_at: None,
            created_at: now,
            updated_at: now,
        };
        let campaigns = self.campaigns();
        let invitations = self.campaign_invitations();
        let invitation_for_result = invitation.clone();
        let gm_account_id = gm_account_id.to_owned();
        let campaign_id = campaign_id.to_owned();
        let result = self
            .with_transaction(move |session| {
                let campaigns = campaigns.clone();
                let invitations = invitations.clone();
                let invitation = invitation.clone();
                let gm_account_id = gm_account_id.clone();
                let campaign_id = campaign_id.clone();
                Box::pin(async move {
                    let campaign = campaigns
                        .find_one(doc! {
                            "_id": &campaign_id,
                            "owner_account_id": &gm_account_id,
                            "lifecycle.state": "open",
                            "members": {
                                "$elemMatch": {
                                    "account_id": &gm_account_id,
                                    "role": "game_master",
                                    "state": "active",
                                }
                            },
                        })
                        .session(&mut *session)
                        .await
                        .map_err(|error| {
                            PersistenceError::mongo("authorize campaign invitation", error)
                        })?;
                    if campaign.is_none() {
                        return Err(PersistenceError::NotFound {
                            entity: "campaign_membership",
                            id: campaign_id,
                        });
                    }
                    invitations
                        .update_many(
                            doc! {
                                "campaign_id": &invitation.campaign_id,
                                "invitee_email_lookup_hmac":
                                    &invitation.invitee_email_lookup_hmac,
                                "state": "active",
                                "expires_at": { "$lte": DateTime::now() },
                            },
                            doc! {
                                "$set": {
                                    "state": "expired",
                                    "updated_at": DateTime::now(),
                                }
                            },
                        )
                        .session(&mut *session)
                        .await
                        .map_err(|error| {
                            PersistenceError::mongo("expire stale campaign invitations", error)
                        })?;
                    invitations
                        .insert_one(invitation)
                        .session(&mut *session)
                        .await
                        .map_err(|error| {
                            PersistenceError::mongo("create campaign invitation", error)
                        })?;
                    Ok(())
                })
            })
            .await;
        if let Err(error) = result {
            return Err(map_transaction_error(
                error,
                "campaign_invitation",
                &invitation_for_result.id,
            ));
        }
        invitation_row(invitation_for_result)
    }

    pub async fn revoke_campaign_invitation(
        &self,
        gm_account_id: &str,
        invitation_id: &str,
    ) -> Result<(), RepositoryError> {
        validate_account_id(gm_account_id)?;
        validate_invitation_id(invitation_id)?;
        let campaigns = self.campaigns();
        let invitations = self.campaign_invitations();
        let gm_account_id = gm_account_id.to_owned();
        let invitation_id = invitation_id.to_owned();
        let invitation_id_for_error = invitation_id.clone();
        self.with_transaction(move |session| {
            let campaigns = campaigns.clone();
            let invitations = invitations.clone();
            let gm_account_id = gm_account_id.clone();
            let invitation_id = invitation_id.clone();
            Box::pin(async move {
                let invitation = invitations
                    .find_one(doc! { "_id": &invitation_id, "state": "active" })
                    .session(&mut *session)
                    .await
                    .map_err(|error| PersistenceError::mongo("load invitation for revoke", error))?
                    .ok_or_else(|| PersistenceError::NotFound {
                        entity: "campaign_invitation",
                        id: invitation_id.clone(),
                    })?;
                let campaign = campaigns
                    .find_one(doc! {
                        "_id": &invitation.campaign_id,
                        "owner_account_id": &gm_account_id,
                        "members": {
                            "$elemMatch": {
                                "account_id": &gm_account_id,
                                "role": "game_master",
                                "state": "active",
                            }
                        },
                    })
                    .session(&mut *session)
                    .await
                    .map_err(|error| {
                        PersistenceError::mongo("authorize invitation revoke", error)
                    })?;
                if campaign.is_none() {
                    return Err(PersistenceError::NotFound {
                        entity: "campaign_invitation",
                        id: invitation_id.clone(),
                    });
                }
                let updated = invitations
                    .update_one(
                        doc! { "_id": &invitation_id, "state": "active" },
                        doc! {
                            "$set": {
                                "state": "revoked",
                                "revoked_at": DateTime::now(),
                                "updated_at": DateTime::now(),
                            }
                        },
                    )
                    .session(&mut *session)
                    .await
                    .map_err(|error| {
                        PersistenceError::mongo("revoke campaign invitation", error)
                    })?;
                if updated.modified_count != 1 {
                    return Err(PersistenceError::NotFound {
                        entity: "campaign_invitation",
                        id: invitation_id,
                    });
                }
                Ok(())
            })
        })
        .await
        .map_err(|error| {
            map_transaction_error(
                error,
                "campaign_invitation",
                invitation_id_for_error.as_str(),
            )
        })
    }

    /// Owner-scoped invitation lookup. Invitation IDs alone never authorize a read.
    pub async fn load_campaign_invitation(
        &self,
        gm_account_id: &str,
        invitation_id: &str,
    ) -> Result<Option<CampaignInvitationRow>, RepositoryError> {
        validate_account_id(gm_account_id)?;
        validate_invitation_id(invitation_id)?;
        let invitation = self
            .campaign_invitations()
            .find_one(doc! {
                "_id": invitation_id,
                "inviter_account_id": gm_account_id,
                "state": "active",
                "expires_at": { "$gt": DateTime::now() },
            })
            .await
            .map_err(|error| mongo_error("load campaign invitation", error))?;
        invitation.map(invitation_row).transpose()
    }

    pub async fn accept_campaign_invitation(
        &self,
        account_id: &str,
        invitation_id: &str,
        invitee_email: &str,
        now_unix_seconds: u64,
    ) -> Result<CampaignMembershipRow, RepositoryError> {
        validate_account_id(account_id)?;
        validate_invitation_id(invitation_id)?;
        validate_email(invitee_email)?;
        require_account(self, account_id).await?;
        let now = epoch_seconds_to_date(now_unix_seconds, "invitation acceptance time")?;
        let campaigns = self.campaigns();
        let invitations = self.campaign_invitations();
        let account_id_owned = account_id.to_owned();
        let invitation_id_owned = invitation_id.to_owned();
        let normalized_email = invitee_email.to_owned();
        let campaign_id = self
            .with_transaction(move |session| {
                let campaigns = campaigns.clone();
                let invitations = invitations.clone();
                let account_id = account_id_owned.clone();
                let invitation_id = invitation_id_owned.clone();
                let invitee_email = normalized_email.clone();
                Box::pin(async move {
                    let invitation = invitations
                        .find_one(doc! {
                            "_id": &invitation_id,
                            "state": "active",
                            "expires_at": { "$gt": now },
                        })
                        .session(&mut *session)
                        .await
                        .map_err(|error| {
                            PersistenceError::mongo("load invitation for accept", error)
                        })?
                        .ok_or_else(|| PersistenceError::NotFound {
                            entity: "campaign_invitation",
                            id: invitation_id.clone(),
                        })?;
                    if invitation.invitee_email_lookup_hmac
                        != invitation_email_digest(&invitation.campaign_id, &invitee_email)
                    {
                        return Err(PersistenceError::NotFound {
                            entity: "campaign_invitation",
                            id: invitation_id,
                        });
                    }
                    let mut campaign = campaigns
                        .find_one(doc! {
                            "_id": &invitation.campaign_id,
                            "owner_account_id": &invitation.inviter_account_id,
                            "lifecycle.state": "open",
                            "members": {
                                "$elemMatch": {
                                    "account_id": &invitation.inviter_account_id,
                                    "role": "game_master",
                                    "state": "active",
                                }
                            },
                        })
                        .session(&mut *session)
                        .await
                        .map_err(|error| {
                            PersistenceError::mongo("load invitation campaign", error)
                        })?
                        .ok_or_else(|| PersistenceError::NotFound {
                            entity: "campaign_invitation",
                            id: invitation.id.clone(),
                        })?;
                    let now = DateTime::now();
                    if let Some(member) = campaign
                        .members
                        .iter_mut()
                        .find(|member| member.account_id == account_id)
                    {
                        if member.state == "active" {
                            return Err(PersistenceError::AlreadyExists {
                                entity: "campaign_membership",
                                id: campaign.id,
                            });
                        }
                        member.role = "player".to_owned();
                        member.state = "active".to_owned();
                        member.inviter_account_id = Some(invitation.inviter_account_id.clone());
                        member.joined_at = now;
                        member.left_at = None;
                        member.updated_at = now;
                    } else {
                        if campaign.members.len() >= MAX_CAMPAIGN_MEMBERS {
                            return Err(PersistenceError::AlreadyExists {
                                entity: "campaign_membership_capacity",
                                id: campaign.id,
                            });
                        }
                        campaign.members.push(CampaignMemberDocument {
                            account_id: account_id.clone(),
                            role: "player".to_owned(),
                            state: "active".to_owned(),
                            inviter_account_id: Some(invitation.inviter_account_id.clone()),
                            joined_at: now,
                            left_at: None,
                            created_at: now,
                            updated_at: now,
                        });
                    }
                    let members = mongodb::bson::to_bson(&campaign.members)
                        .map_err(PersistenceError::BsonEncoding)?;
                    let campaign_update = campaigns
                        .update_one(
                            doc! { "_id": &campaign.id, "revision": campaign.revision },
                            doc! {
                                "$set": {
                                    "members": members,
                                    "updated_at": now,
                                },
                                "$inc": { "revision": 1_i64 },
                            },
                        )
                        .session(&mut *session)
                        .await
                        .map_err(|error| {
                            PersistenceError::mongo("accept campaign membership", error)
                        })?;
                    if campaign_update.modified_count != 1 {
                        return Err(PersistenceError::RevisionConflict {
                            entity: "campaign",
                            id: campaign.id,
                            expected: nonnegative_u64(campaign.revision),
                            actual: nonnegative_u64(campaign.revision).saturating_add(1),
                        });
                    }
                    let invitation_update = invitations
                        .update_one(
                            doc! {
                                "_id": &invitation.id,
                                "state": "active",
                                "expires_at": { "$gt": now },
                            },
                            doc! {
                                "$set": {
                                    "state": "accepted",
                                    "accepted_account_id": &account_id,
                                    "accepted_at": now,
                                    "updated_at": now,
                                }
                            },
                        )
                        .session(&mut *session)
                        .await
                        .map_err(|error| {
                            PersistenceError::mongo("consume campaign invitation", error)
                        })?;
                    if invitation_update.modified_count != 1 {
                        return Err(PersistenceError::NotFound {
                            entity: "campaign_invitation",
                            id: invitation.id,
                        });
                    }
                    Ok(campaign.id)
                })
            })
            .await
            .map_err(|error| map_transaction_error(error, "campaign_invitation", invitation_id))?;
        self.load_membership(&campaign_id, account_id)
            .await?
            .ok_or_else(|| RepositoryError::NotFound {
                entity: "campaign_membership",
                id: campaign_id,
            })
    }

    pub async fn load_membership(
        &self,
        campaign_id: &str,
        account_id: &str,
    ) -> Result<Option<CampaignMembershipRow>, RepositoryError> {
        validate_campaign_id(campaign_id)?;
        validate_account_id(account_id)?;
        let campaign = self
            .campaigns()
            .find_one(doc! {
                "_id": campaign_id,
                "members.account_id": account_id,
            })
            .await
            .map_err(|error| mongo_error("load campaign membership", error))?;
        campaign
            .and_then(|campaign| {
                campaign
                    .members
                    .into_iter()
                    .find(|member| member.account_id == account_id)
                    .map(|member| membership_row(campaign_id, member))
            })
            .transpose()
    }

    pub async fn is_active_member(
        &self,
        account_id: &str,
        campaign_id: &str,
    ) -> Result<bool, RepositoryError> {
        validate_account_id(account_id)?;
        validate_campaign_id(campaign_id)?;
        let campaign = self
            .campaigns()
            .find_one(doc! {
                "_id": campaign_id,
                "members": {
                    "$elemMatch": {
                        "account_id": account_id,
                        "state": "active",
                    }
                },
            })
            .projection(doc! { "_id": 1 })
            .await
            .map_err(|error| mongo_error("check active campaign membership", error))?;
        Ok(campaign.is_some())
    }

    pub async fn load_member_campaign_summary(
        &self,
        account_id: &str,
        campaign_id: &str,
    ) -> Result<Option<crate::repository::lifecycle::CampaignSummary>, RepositoryError> {
        validate_account_id(account_id)?;
        validate_campaign_id(campaign_id)?;
        self.campaigns()
            .find_one(doc! {
                "_id": campaign_id,
                "members": {
                    "$elemMatch": {
                        "account_id": account_id,
                        "state": "active",
                    }
                },
            })
            .await
            .map_err(|error| mongo_error("load member campaign summary", error))?
            .map(crate::repository::lifecycle::campaign_summary_from_document)
            .transpose()
    }

    pub async fn list_campaign_members(
        &self,
        account_id: &str,
        campaign_id: &str,
    ) -> Result<Vec<CampaignMembershipRow>, RepositoryError> {
        validate_account_id(account_id)?;
        validate_campaign_id(campaign_id)?;
        let campaign = self
            .campaigns()
            .find_one(doc! {
                "_id": campaign_id,
                "members": {
                    "$elemMatch": {
                        "account_id": account_id,
                        "state": "active",
                    }
                },
            })
            .await
            .map_err(|error| mongo_error("list campaign members", error))?
            .ok_or_else(|| RepositoryError::NotFound {
                entity: "campaign_membership",
                id: campaign_id.to_owned(),
            })?;
        campaign
            .members
            .into_iter()
            .map(|member| membership_row(campaign_id, member))
            .collect()
    }

    pub async fn list_account_campaigns(
        &self,
        account_id: &str,
    ) -> Result<Vec<MembershipCampaignSummary>, RepositoryError> {
        validate_account_id(account_id)?;
        let mut cursor = self
            .campaigns()
            .find(doc! {
                "members": {
                    "$elemMatch": {
                        "account_id": account_id,
                        "state": "active",
                    }
                },
            })
            .sort(doc! { "updated_at": -1, "_id": 1 })
            .await
            .map_err(|error| mongo_error("list account campaigns", error))?;
        let mut output = Vec::new();
        while cursor
            .advance()
            .await
            .map_err(|error| mongo_error("read account campaigns", error))?
        {
            let campaign = cursor
                .deserialize_current()
                .map_err(|error| mongo_error("decode account campaigns", error))?;
            let member =
                campaign
                    .active_member(account_id)
                    .ok_or_else(|| RepositoryError::NotFound {
                        entity: "campaign_membership",
                        id: campaign.id.clone(),
                    })?;
            output.push(MembershipCampaignSummary {
                campaign_id: campaign.id.clone(),
                title: campaign.title.clone(),
                theme_id: campaign.theme_id.clone(),
                role: MembershipRole::try_from_str(&member.role)?,
                state: MembershipState::try_from_str(&member.state)?,
                created_at: date_string(member.created_at, "campaigns")?,
                updated_at: date_string(member.updated_at, "campaigns")?,
            });
        }
        Ok(output)
    }

    pub async fn remove_campaign_member(
        &self,
        gm_account_id: &str,
        campaign_id: &str,
        member_account_id: &str,
    ) -> Result<(), RepositoryError> {
        validate_account_id(gm_account_id)?;
        validate_campaign_id(campaign_id)?;
        validate_account_id(member_account_id)?;
        if gm_account_id == member_account_id {
            return invalid(
                "campaign_membership",
                campaign_id,
                "a game master cannot remove their own ownership membership",
            );
        }
        let campaigns = self.campaigns();
        let instances = self.campaign_character_instances();
        let gm_account_id = gm_account_id.to_owned();
        let campaign_id = campaign_id.to_owned();
        let member_account_id = member_account_id.to_owned();
        let campaign_id_for_error = campaign_id.clone();
        self.with_transaction(move |session| {
            let campaigns = campaigns.clone();
            let instances = instances.clone();
            let gm_account_id = gm_account_id.clone();
            let campaign_id = campaign_id.clone();
            let member_account_id = member_account_id.clone();
            Box::pin(async move {
                let mut campaign = campaigns
                    .find_one(doc! {
                        "_id": &campaign_id,
                        "owner_account_id": &gm_account_id,
                        "members": {
                            "$elemMatch": {
                                "account_id": &gm_account_id,
                                "role": "game_master",
                                "state": "active",
                            }
                        },
                    })
                    .session(&mut *session)
                    .await
                    .map_err(|error| PersistenceError::mongo("authorize member removal", error))?
                    .ok_or_else(|| PersistenceError::NotFound {
                        entity: "campaign_membership",
                        id: campaign_id.clone(),
                    })?;
                let now = DateTime::now();
                let target = campaign
                    .members
                    .iter_mut()
                    .find(|member| {
                        member.account_id == member_account_id && member.state == "active"
                    })
                    .ok_or_else(|| PersistenceError::NotFound {
                        entity: "campaign_membership",
                        id: member_account_id.clone(),
                    })?;
                target.state = "removed".to_owned();
                target.left_at = Some(now);
                target.updated_at = now;
                let members = mongodb::bson::to_bson(&campaign.members)
                    .map_err(PersistenceError::BsonEncoding)?;
                let update = campaigns
                    .update_one(
                        doc! { "_id": &campaign_id, "revision": campaign.revision },
                        doc! {
                            "$set": {
                                "members": members,
                                "updated_at": now,
                            },
                            "$inc": { "revision": 1_i64 },
                        },
                    )
                    .session(&mut *session)
                    .await
                    .map_err(|error| PersistenceError::mongo("remove campaign member", error))?;
                if update.modified_count != 1 {
                    return Err(PersistenceError::RevisionConflict {
                        entity: "campaign",
                        id: campaign_id.clone(),
                        expected: nonnegative_u64(campaign.revision),
                        actual: nonnegative_u64(campaign.revision).saturating_add(1),
                    });
                }
                instances
                    .update_many(
                        doc! {
                            "campaign_id": &campaign_id,
                            "account_id": &member_account_id,
                            "state": "active",
                        },
                        doc! {
                            "$set": {
                                "state": "retired",
                                "retired_at": now,
                                "updated_at": now,
                            },
                            "$inc": { "revision": 1_i64 },
                        },
                    )
                    .session(&mut *session)
                    .await
                    .map_err(|error| {
                        PersistenceError::mongo("retire removed member character", error)
                    })?;
                Ok(())
            })
        })
        .await
        .map_err(|error| {
            map_transaction_error(error, "campaign_membership", campaign_id_for_error.as_str())
        })
    }

    pub async fn load_campaign_theme_for_member(
        &self,
        account_id: &str,
        campaign_id: &str,
    ) -> Result<Option<String>, RepositoryError> {
        validate_account_id(account_id)?;
        validate_campaign_id(campaign_id)?;
        Ok(self
            .campaigns()
            .find_one(doc! {
                "_id": campaign_id,
                "members": {
                    "$elemMatch": {
                        "account_id": account_id,
                        "state": "active",
                    }
                },
            })
            .await
            .map_err(|error| mongo_error("load campaign theme", error))?
            .map(|campaign| campaign.theme_id))
    }

    pub async fn assign_character_to_campaign(
        &self,
        account_id: &str,
        campaign_id: &str,
        player_character_id: &str,
        source_character: &PlayerCharacter,
    ) -> Result<AssignCharacterOutcome, RepositoryError> {
        validate_account_id(account_id)?;
        validate_campaign_id(campaign_id)?;
        validate_character_id(player_character_id)?;
        if source_character.owner_account_id != account_id
            || source_character.character_id != player_character_id
        {
            return Err(RepositoryError::NotFound {
                entity: "player_character",
                id: player_character_id.to_owned(),
            });
        }
        source_character
            .validate()
            .map_err(|source| RepositoryError::HeroValidation {
                entity: "player_character",
                id: player_character_id.to_owned(),
                source,
            })?;

        let authoritative = self
            .store()
            .collection::<PlayerCharacterDocument>(CollectionName::PlayerCharacters)
            .find_one(doc! {
                "_id": player_character_id,
                "owner_account_id": account_id,
            })
            .await
            .map_err(|error| mongo_error("load source player character", error))?
            .map(player_character_from_document)
            .transpose()?
            .ok_or_else(|| RepositoryError::NotFound {
                entity: "player_character",
                id: player_character_id.to_owned(),
            })?;
        if &authoritative != source_character {
            return Err(RepositoryError::RevisionConflict {
                entity: "player_character",
                id: player_character_id.to_owned(),
                expected: source_character.revision,
                actual: authoritative.revision,
            });
        }
        let campaign_theme = self
            .load_campaign_theme_for_member(account_id, campaign_id)
            .await?
            .ok_or_else(|| RepositoryError::NotFound {
                entity: "campaign_membership",
                id: campaign_id.to_owned(),
            })?;
        if campaign_theme != theme_id_str(authoritative.theme_id()) {
            return invalid(
                "campaign_character_instance",
                campaign_id,
                "character theme does not match campaign theme",
            );
        }

        let instance_id = format!("campaign-character:{}", Uuid::new_v4());
        let runtime_hero_id = format!("campaign-character-runtime:{}", Uuid::new_v4());
        let hero = authoritative
            .instantiate_for_campaign(campaign_id.to_owned(), runtime_hero_id.clone())
            .map_err(|source| RepositoryError::HeroValidation {
                entity: "campaign_character_instance",
                id: instance_id.clone(),
                source,
            })?;
        let source_digest = player_character_digest(&authoritative)?;
        let captured_at = DateTime::now();
        let instance = CampaignCharacterInstanceDocument {
            id: instance_id.clone(),
            schema_version: 1,
            revision: 1,
            campaign_id: campaign_id.to_owned(),
            account_id: account_id.to_owned(),
            source_player_character_id: player_character_id.to_owned(),
            runtime_hero_character_id: runtime_hero_id.clone(),
            state: "active".to_owned(),
            source_snapshot: SourceCharacterSnapshot {
                source_revision: to_i64(authoritative.revision, "source revision")?,
                source_schema_version: i64::from(authoritative.schema_version),
                source_digest,
                captured_at,
                display_name: authoritative.display_name.clone(),
                player_character: authoritative.clone(),
            },
            progression: CampaignProgressionDocument {
                level: i64::from(hero.level.value()),
                experience_points: i64::from(hero.experience_points),
                milestone_count: 0,
            },
            runtime: CampaignRuntimeDocument {
                current_hit_points: i64::from(hero.sheet.current_hit_points),
                maximum_hit_points: i64::from(hero.sheet.maximum_hit_points),
                temporary_hit_points: 0,
                bde_balance: 3,
                hero,
            },
            created_at: captured_at,
            updated_at: captured_at,
            retired_at: None,
        };
        let campaigns = self.campaigns();
        let characters = self
            .store()
            .collection::<PlayerCharacterDocument>(CollectionName::PlayerCharacters);
        let instances = self.campaign_character_instances();
        let account_id_owned = account_id.to_owned();
        let campaign_id_owned = campaign_id.to_owned();
        let character_id_owned = player_character_id.to_owned();
        let source_revision = to_i64(authoritative.revision, "source revision")?;
        let result = self
            .with_transaction(move |session| {
                let campaigns = campaigns.clone();
                let characters = characters.clone();
                let instances = instances.clone();
                let instance = instance.clone();
                let account_id = account_id_owned.clone();
                let campaign_id = campaign_id_owned.clone();
                let character_id = character_id_owned.clone();
                Box::pin(async move {
                    let campaign = campaigns
                        .find_one(doc! {
                            "_id": &campaign_id,
                            "lifecycle.state": "open",
                            "theme_id": theme_id_str(
                                instance.source_snapshot.player_character.theme_id()
                            ),
                            "members": {
                                "$elemMatch": {
                                    "account_id": &account_id,
                                    "state": "active",
                                }
                            },
                        })
                        .session(&mut *session)
                        .await
                        .map_err(|error| {
                            PersistenceError::mongo("authorize character assignment", error)
                        })?;
                    if campaign.is_none() {
                        return Err(PersistenceError::NotFound {
                            entity: "campaign_membership",
                            id: campaign_id,
                        });
                    }
                    let source = characters
                        .find_one(doc! {
                            "_id": &character_id,
                            "owner_account_id": &account_id,
                            "revision": source_revision,
                        })
                        .session(&mut *session)
                        .await
                        .map_err(|error| {
                            PersistenceError::mongo("recheck source character", error)
                        })?;
                    if source.is_none() {
                        return Err(PersistenceError::NotFound {
                            entity: "player_character",
                            id: character_id,
                        });
                    }
                    instances
                        .insert_one(instance)
                        .session(&mut *session)
                        .await
                        .map_err(|error| {
                            PersistenceError::mongo("assign campaign character", error)
                        })?;
                    Ok(())
                })
            })
            .await;
        if let Err(error) = result {
            return Err(map_transaction_error(
                error,
                "campaign_character_instance",
                campaign_id,
            ));
        }
        Ok(AssignCharacterOutcome {
            instance_id,
            runtime_hero_character_id: runtime_hero_id,
        })
    }

    pub async fn load_active_character_instance(
        &self,
        account_id: &str,
        campaign_id: &str,
    ) -> Result<Option<CampaignCharacterInstanceRow>, RepositoryError> {
        validate_account_id(account_id)?;
        validate_campaign_id(campaign_id)?;
        self.campaign_character_instances()
            .find_one(doc! {
                "campaign_id": campaign_id,
                "account_id": account_id,
                "state": "active",
            })
            .await
            .map_err(|error| mongo_error("load active campaign character", error))?
            .map(character_instance_row)
            .transpose()
    }

    /// Loads runtime state only through an active same-campaign membership.
    pub async fn load_runtime_hero_character(
        &self,
        account_id: &str,
        campaign_id: &str,
        hero_character_id: &str,
    ) -> Result<Option<HeroCharacter>, RepositoryError> {
        validate_account_id(account_id)?;
        validate_campaign_id(campaign_id)?;
        if !hero_character_id.starts_with("campaign-character-runtime:")
            || !is_valid_opaque_id(hero_character_id)
        {
            return invalid(
                "campaign_character_instance",
                hero_character_id,
                "runtime character id must be a prefixed opaque identifier",
            );
        }
        if !self.is_active_member(account_id, campaign_id).await? {
            return Ok(None);
        }
        let stored = self
            .campaign_character_instances()
            .find_one(doc! {
                "campaign_id": campaign_id,
                "runtime_hero_character_id": hero_character_id,
                "state": "active",
            })
            .await
            .map_err(|error| mongo_error("load authorized runtime character", error))?;
        let Some(stored) = stored else {
            return Ok(None);
        };
        let caller_is_owner = stored.account_id == account_id;
        let caller_is_gm = self
            .campaigns()
            .find_one(doc! {
                "_id": campaign_id,
                "members": {
                    "$elemMatch": {
                        "account_id": account_id,
                        "role": "game_master",
                        "state": "active",
                    }
                },
            })
            .projection(doc! { "_id": 1 })
            .await
            .map_err(|error| mongo_error("authorize runtime character", error))?
            .is_some();
        if !caller_is_owner && !caller_is_gm {
            return Ok(None);
        }
        stored
            .runtime
            .hero
            .validate()
            .map_err(|source| RepositoryError::HeroValidation {
                entity: "campaign_character_instance",
                id: stored.id,
                source,
            })?;
        Ok(Some(stored.runtime.hero))
    }

    pub(crate) fn campaign_character_instances(
        &self,
    ) -> Collection<CampaignCharacterInstanceDocument> {
        self.store()
            .collection(CollectionName::CampaignCharacterInstances)
    }

    fn campaign_invitations(&self) -> Collection<CampaignInvitationDocument> {
        self.store().collection(CollectionName::CampaignInvitations)
    }
}

async fn require_account(
    repository: &MongoRepository,
    account_id: &str,
) -> Result<(), RepositoryError> {
    let exists = repository
        .store()
        .document_collection(CollectionName::Accounts)
        .find_one(doc! { "_id": account_id })
        .projection(doc! { "_id": 1 })
        .await
        .map_err(|error| mongo_error("load campaign account", error))?;
    if exists.is_none() {
        return Err(RepositoryError::NotFound {
            entity: "account",
            id: account_id.to_owned(),
        });
    }
    Ok(())
}

fn membership_row(
    campaign_id: &str,
    member: CampaignMemberDocument,
) -> Result<CampaignMembershipRow, RepositoryError> {
    Ok(CampaignMembershipRow {
        campaign_id: campaign_id.to_owned(),
        account_id: member.account_id,
        role: MembershipRole::try_from_str(&member.role)?,
        state: MembershipState::try_from_str(&member.state)?,
        created_at: date_string(member.created_at, "campaigns")?,
        updated_at: date_string(member.updated_at, "campaigns")?,
    })
}

fn invitation_row(
    invitation: CampaignInvitationDocument,
) -> Result<CampaignInvitationRow, RepositoryError> {
    Ok(CampaignInvitationRow {
        id: invitation.id,
        campaign_id: invitation.campaign_id,
        inviter_account_id: invitation.inviter_account_id,
        invitee_email_digest: invitation.invitee_email_lookup_hmac,
        expires_at: date_string(invitation.expires_at, "campaign_invitations")?,
        created_at: date_string(invitation.created_at, "campaign_invitations")?,
    })
}

fn character_instance_row(
    stored: CampaignCharacterInstanceDocument,
) -> Result<CampaignCharacterInstanceRow, RepositoryError> {
    Ok(CampaignCharacterInstanceRow {
        campaign_id: stored.campaign_id,
        account_id: stored.account_id,
        instance_id: stored.id,
        source_player_character_id: stored.source_player_character_id,
        runtime_hero_character_id: stored.runtime_hero_character_id,
        source_display_name: stored.source_snapshot.display_name,
        source_choices_digest: stored.source_snapshot.source_digest,
        state: CharacterInstanceState::try_from_str(&stored.state)?,
        created_at: date_string(stored.created_at, "campaign_character_instances")?,
        retired_at: stored
            .retired_at
            .map(|date| date_string(date, "campaign_character_instances"))
            .transpose()?,
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

fn validate_campaign_id(campaign_id: &str) -> Result<(), RepositoryError> {
    if !campaign_id.starts_with("campaign:") || !is_valid_opaque_id(campaign_id) {
        return invalid(
            "campaign_session",
            campaign_id,
            "campaign identifier is invalid",
        );
    }
    Ok(())
}

fn validate_character_id(character_id: &str) -> Result<(), RepositoryError> {
    if !character_id.starts_with("character:") || !is_valid_opaque_id(character_id) {
        return invalid(
            "player_character",
            character_id,
            "character identifier is invalid",
        );
    }
    Ok(())
}

fn validate_invitation_id(invitation_id: &str) -> Result<(), RepositoryError> {
    if !invitation_id.starts_with("invitation:") || !is_valid_opaque_id(invitation_id) {
        return invalid(
            "campaign_invitation",
            invitation_id,
            "invitation identifier is invalid",
        );
    }
    Ok(())
}

fn validate_campaign_title(title: &str) -> Result<(), RepositoryError> {
    if title.trim().is_empty()
        || title.chars().count() > MAX_CAMPAIGN_TITLE_LEN
        || title.chars().any(char::is_control)
    {
        return invalid(
            "campaign_session",
            title,
            "campaign title must be 1-200 non-control characters",
        );
    }
    Ok(())
}

fn validate_theme_id(theme_id: &str) -> Result<(), RepositoryError> {
    if theme_id != "dev.manchester-arcana.rainbound-borough"
        && theme_id != "dev.manchester-arcana.emberline-archive"
    {
        return invalid(
            "campaign_session",
            theme_id,
            "theme id is not a supported MVP theme",
        );
    }
    Ok(())
}

fn validate_email(email: &str) -> Result<(), RepositoryError> {
    let normalized = email.trim().to_ascii_lowercase();
    if normalized.is_empty()
        || normalized.len() > MAX_EMAIL_LEN
        || !normalized.contains('@')
        || normalized.chars().any(char::is_whitespace)
    {
        return invalid("campaign_invitation", email, "invitee email is invalid");
    }
    Ok(())
}

fn theme_id_str(theme: manchester_dnd_core::hero::ThemeId) -> &'static str {
    match theme {
        manchester_dnd_core::hero::ThemeId::RainboundBorough => {
            "dev.manchester-arcana.rainbound-borough"
        }
        manchester_dnd_core::hero::ThemeId::EmberlineArchive => {
            "dev.manchester-arcana.emberline-archive"
        }
    }
}

fn invitation_email_digest(campaign_id: &str, email: &str) -> String {
    let normalized = email.trim().to_ascii_lowercase();
    let mut hasher = Sha256::new();
    hasher.update(b"campaign-invitation-email/v1\0");
    hasher.update(campaign_id.as_bytes());
    hasher.update(b"\0");
    hasher.update(normalized.as_bytes());
    format!("sha256:{:x}", hasher.finalize())
}

fn player_character_digest(character: &PlayerCharacter) -> Result<String, RepositoryError> {
    let encoded = serde_json::to_vec(character).map_err(|source| RepositoryError::Serialize {
        entity: "player character source snapshot",
        source,
    })?;
    Ok(format!("sha256:{:x}", Sha256::digest(encoded)))
}

fn normalize_title(title: &str) -> String {
    title.trim().to_lowercase()
}

fn epoch_seconds_to_date(value: u64, field: &'static str) -> Result<DateTime, RepositoryError> {
    let milliseconds = value
        .checked_mul(1_000)
        .and_then(|value| i64::try_from(value).ok())
        .ok_or(RepositoryError::NumericRange { field })?;
    Ok(DateTime::from_millis(milliseconds))
}

fn to_i64(value: u64, field: &'static str) -> Result<i64, RepositoryError> {
    i64::try_from(value).map_err(|_| RepositoryError::NumericRange { field })
}

fn nonnegative_u64(value: i64) -> u64 {
    if value < 0 { 0 } else { value as u64 }
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

fn map_transaction_error(
    error: PersistenceError,
    entity: &'static str,
    id: &str,
) -> RepositoryError {
    match error {
        PersistenceError::NotFound {
            entity: stored_entity,
            id,
        } => RepositoryError::NotFound {
            entity: stored_entity,
            id,
        },
        PersistenceError::AlreadyExists {
            entity: stored_entity,
            id,
        } => RepositoryError::AlreadyExists {
            entity: stored_entity,
            id,
        },
        PersistenceError::RevisionConflict {
            entity: stored_entity,
            id,
            expected,
            actual,
        } => RepositoryError::RevisionConflict {
            entity: stored_entity,
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

    use manchester_dnd_core::hero::{
        AncestryId, BackgroundId, BackgroundSelection, ClassSelection, EquipmentId,
        EquipmentSelection, FightingStyleId, HeroChoices, HeroConceptId, HeroPins,
        HeroPresentation, SkillId, StandardArrayAssignment, ThemeId,
    };
    use mongodb::bson::doc;

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
        let database = format!("mdnd_test_memberships_{}", Uuid::new_v4().simple());
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
                name: "Mara".to_owned(),
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
    async fn mongo_membership_contract_covers_auth_invites_and_immutable_instances() {
        let Some((repository, database)) = test_repository().await else {
            return;
        };
        let gm = format!("account:{}", Uuid::new_v4());
        let player = format!("account:{}", Uuid::new_v4());
        let outsider = format!("account:{}", Uuid::new_v4());
        insert_account(&repository, &gm).await;
        insert_account(&repository, &player).await;
        insert_account(&repository, &outsider).await;

        let campaign = repository
            .create_campaign_with_owner(
                &gm,
                "Rain over Ancoats",
                "dev.manchester-arcana.rainbound-borough",
            )
            .await
            .expect("campaign creation must work");
        assert_eq!(
            repository
                .create_campaign_with_owner(
                    &gm,
                    "Rain over Ancoats",
                    "dev.manchester-arcana.rainbound-borough",
                )
                .await
                .expect("campaign create replay must work"),
            campaign
        );
        assert!(
            repository
                .list_campaign_members(&outsider, &campaign.campaign_id)
                .await
                .is_err()
        );
        assert!(matches!(
            repository
                .remove_campaign_member(&gm, &campaign.campaign_id, &gm)
                .await,
            Err(RepositoryError::InvalidDomainState { .. })
        ));

        let now = u64::try_from(DateTime::now().timestamp_millis())
            .expect("test clock must be positive")
            / 1_000;
        let invitation = repository
            .create_campaign_invitation(
                &gm,
                &campaign.campaign_id,
                "player@example.com",
                now + 3_600,
            )
            .await
            .expect("invitation creation must work");
        assert!(matches!(
            repository
                .create_campaign_invitation(
                    &gm,
                    &campaign.campaign_id,
                    "player@example.com",
                    now + 3_600,
                )
                .await,
            Err(RepositoryError::AlreadyExists { .. })
        ));
        let raw_invitation = repository
            .store()
            .document_collection(CollectionName::CampaignInvitations)
            .find_one(doc! { "_id": &invitation.id })
            .await
            .expect("invitation read must work")
            .expect("invitation must exist");
        assert!(
            !raw_invitation
                .values()
                .any(|value| value.as_str() == Some("player@example.com"))
        );
        assert!(!raw_invitation.contains_key("invitee_email"));
        assert!(!raw_invitation.contains_key("invite_token"));
        let revoked = repository
            .create_campaign_invitation(
                &gm,
                &campaign.campaign_id,
                "revoked@example.com",
                now + 3_600,
            )
            .await
            .expect("second invitation must work");
        repository
            .revoke_campaign_invitation(&gm, &revoked.id)
            .await
            .expect("revocation must work");
        assert!(
            repository
                .accept_campaign_invitation(&outsider, &revoked.id, "revoked@example.com", now,)
                .await
                .is_err()
        );
        repository
            .accept_campaign_invitation(&player, &invitation.id, "player@example.com", now)
            .await
            .expect("invitation acceptance must work");
        assert!(
            repository
                .accept_campaign_invitation(&outsider, &invitation.id, "player@example.com", now,)
                .await
                .is_err()
        );

        let character_id = format!("character:{}", Uuid::new_v4());
        let character = PlayerCharacter::new(
            character_id.clone(),
            player.clone(),
            "Mara".to_owned(),
            choices(),
        )
        .expect("character fixture must validate");
        repository
            .create_player_character(&player, &character)
            .await
            .expect("library character must save");
        let assigned = repository
            .assign_character_to_campaign(&player, &campaign.campaign_id, &character_id, &character)
            .await
            .expect("assignment must work");
        assert!(matches!(
            repository
                .assign_character_to_campaign(
                    &player,
                    &campaign.campaign_id,
                    &character_id,
                    &character,
                )
                .await,
            Err(RepositoryError::AlreadyExists { .. })
        ));
        let raw_library = repository
            .store()
            .document_collection(CollectionName::PlayerCharacters)
            .find_one(doc! { "_id": &character_id })
            .await
            .expect("library read must work")
            .expect("library character must exist");
        assert!(!raw_library.contains_key("level"));
        let raw_instance = repository
            .store()
            .document_collection(CollectionName::CampaignCharacterInstances)
            .find_one(doc! { "_id": &assigned.instance_id })
            .await
            .expect("instance read must work")
            .expect("instance must exist");
        assert!(raw_instance.get_document("progression").is_ok());
        assert!(raw_instance.get_document("runtime").is_ok());
        assert!(raw_instance.get_document("source_snapshot").is_ok());
        assert!(
            repository
                .load_runtime_hero_character(
                    &outsider,
                    &campaign.campaign_id,
                    &assigned.runtime_hero_character_id,
                )
                .await
                .expect("foreign runtime read must be safe")
                .is_none()
        );

        assert!(
            database.starts_with("mdnd_test_memberships_"),
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
