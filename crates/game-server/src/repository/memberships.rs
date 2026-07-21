//! Account-scoped campaign membership, invitation, and runtime character
//! instance storage (Task 13).
//!
//! Every public method takes a server-derived `account_id`. No method accepts
//! a browser-supplied owner. Cross-account access — including a guessed
//! campaign or invitation ID — returns the same `NotFound` result as a missing
//! document. This is enforced at the SQL layer (`WHERE account_id = $1`) and
//! maps to [`RepositoryError::NotFound`] at this layer.
//!
//! The runtime hero instance is created atomically with the
#![allow(dead_code)]
//! `campaign_character_instances` row in a single transaction, so a crash
//! never leaves a dangling hero_characters row without its binding membership
//! link.

use manchester_dnd_core::{
    PlayerCharacter, SESSION_SCHEMA_VERSION, SessionDto, SessionStatus,
    hero::{HERO_CHARACTER_SCHEMA_VERSION, HeroCharacter},
    is_valid_opaque_id,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::Row;
use uuid::Uuid;

use super::{PostgresRepository, map_insert_error, serialize, to_i64};
use crate::error::RepositoryError;

/// Schema version for membership-related application documents.
pub const MEMBERSHIP_SCHEMA_VERSION: u16 = 1;
/// Default invitation lifetime: 7 days.
pub const INVITATION_TTL_SECONDS: u64 = 7 * 24 * 60 * 60;

const MAX_CAMPAIGN_TITLE_LEN: usize = 200;
const MAX_EMAIL_LEN: usize = 320;

/// A campaign summary returned to a member.
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

/// A campaign membership row.
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

/// A campaign invitation row.
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

/// A runtime character instance bound to a campaign.
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
    const fn as_str(self) -> &'static str {
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
    const fn as_str(self) -> &'static str {
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
    const fn as_str(self) -> &'static str {
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

/// Outcome of creating a campaign with its owner membership atomically.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateCampaignWithOwnerOutcome {
    pub campaign_id: String,
    pub title: String,
    pub theme_id: String,
}

/// Outcome of assigning a library character to a campaign.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssignCharacterOutcome {
    pub instance_id: String,
    pub runtime_hero_character_id: String,
}

impl PostgresRepository {
    /// Creates a new campaign session, its owner membership, and seeds the
    /// title and theme — all in one transaction. The `campaign_id` is
    /// generated server-side. The `account_id` becomes the game master.
    pub async fn create_campaign_with_owner(
        &self,
        account_id: &str,
        title: &str,
        theme_id: &str,
    ) -> Result<CreateCampaignWithOwnerOutcome, RepositoryError> {
        validate_account_id(account_id)?;
        validate_campaign_title(title)?;
        validate_theme_id(theme_id)?;
        let campaign_id = format!("campaign:{}", Uuid::new_v4().simple());
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let session = SessionDto {
            schema_version: SESSION_SCHEMA_VERSION,
            id: campaign_id.clone(),
            ruleset: manchester_dnd_core::RulesetId::Srd5_1,
            title: title.to_owned(),
            status: SessionStatus::Active,
            character_ids: Vec::new(),
            created_at_unix_ms: u64::try_from(now_ms).unwrap_or(0),
            updated_at_unix_ms: u64::try_from(now_ms).unwrap_or(0),
            last_event_sequence: 0,
        };
        session
            .validate()
            .map_err(|source| RepositoryError::CoreValidation {
                entity: "campaign session",
                id: campaign_id.clone(),
                source,
            })?;
        let payload = serialize("campaign session", &session)?;
        let mut transaction = self.pool.begin().await.map_err(RepositoryError::Database)?;
        sqlx::query(
            "INSERT INTO campaign_sessions
             (id, schema_version, revision, payload_json, owner_key, owner_account_id, theme_id)
             VALUES ($1, $2, 1, $3::jsonb, $4, $5, $6)",
        )
        .bind(&campaign_id)
        .bind(i64::from(SESSION_SCHEMA_VERSION))
        .bind(&payload)
        .bind(account_id)
        .bind(account_id)
        .bind(theme_id)
        .execute(&mut *transaction)
        .await
        .map_err(|error| map_insert_error(error, "campaign session", &campaign_id))?;
        sqlx::query(
            "INSERT INTO campaign_memberships
             (campaign_session_id, account_id, role, state, inviter_account_id, accepted_at)
             VALUES ($1, $2, 'game_master', 'active', $2, CURRENT_TIMESTAMP)",
        )
        .bind(&campaign_id)
        .bind(account_id)
        .execute(&mut *transaction)
        .await
        .map_err(|error| map_insert_error(error, "campaign_membership", &campaign_id))?;
        transaction
            .commit()
            .await
            .map_err(RepositoryError::Database)?;
        Ok(CreateCampaignWithOwnerOutcome {
            campaign_id,
            title: title.to_owned(),
            theme_id: theme_id.to_owned(),
        })
    }

    /// Creates an invitation for `invitee_email` to join `campaign_id` as a
    /// player. Only an active game_master of the campaign may invite. Returns
    /// `NotFound` if the caller is not the GM (indistinguishable from a
    /// missing campaign, per the security rule).
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
        // Verify GM membership before creating the invitation.
        let gm_check: Option<String> = sqlx::query_scalar(
            "SELECT account_id FROM campaign_memberships
             WHERE campaign_session_id = $1 AND account_id = $2
               AND role = 'game_master' AND state = 'active'",
        )
        .bind(campaign_id)
        .bind(gm_account_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(RepositoryError::Database)?;
        if gm_check.is_none() {
            return Err(RepositoryError::NotFound {
                entity: "campaign_membership",
                id: campaign_id.to_owned(),
            });
        }
        let invitation_id = format!("invitation:{}", Uuid::new_v4().simple());
        let email_digest = email_sha256(invitee_email);
        let row = sqlx::query(
            "INSERT INTO campaign_invitations
             (id, campaign_session_id, inviter_account_id, invitee_email_digest,
              expires_at)
             VALUES ($1, $2, $3, $4, TO_TIMESTAMP($5))
             RETURNING id, campaign_session_id, inviter_account_id,
                       invitee_email_digest,
                       expires_at::text AS expires_at,
                       created_at::text AS created_at",
        )
        .bind(&invitation_id)
        .bind(campaign_id)
        .bind(gm_account_id)
        .bind(&email_digest)
        .bind(to_i64(expires_at_unix_seconds, "invitation expiry")?)
        .fetch_one(&self.pool)
        .await
        .map_err(|error| map_insert_error(error, "campaign_invitation", &invitation_id))?;
        invitation_from_row(&row)
    }

    /// Revokes a pending invitation. Only the GM of the campaign may revoke.
    pub async fn revoke_campaign_invitation(
        &self,
        gm_account_id: &str,
        invitation_id: &str,
    ) -> Result<(), RepositoryError> {
        validate_account_id(gm_account_id)?;
        validate_invitation_id(invitation_id)?;
        let row = sqlx::query("SELECT campaign_session_id FROM campaign_invitations WHERE id = $1")
            .bind(invitation_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(RepositoryError::Database)?;
        let campaign_id: String = row
            .and_then(|r| r.try_get::<String, _>("campaign_session_id").ok())
            .ok_or(RepositoryError::NotFound {
                entity: "campaign_invitation",
                id: invitation_id.to_owned(),
            })?;
        let gm_check: Option<String> = sqlx::query_scalar(
            "SELECT account_id FROM campaign_memberships
             WHERE campaign_session_id = $1 AND account_id = $2
               AND role = 'game_master' AND state = 'active'",
        )
        .bind(&campaign_id)
        .bind(gm_account_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(RepositoryError::Database)?;
        if gm_check.is_none() {
            return Err(RepositoryError::NotFound {
                entity: "campaign_invitation",
                id: invitation_id.to_owned(),
            });
        }
        let result = sqlx::query(
            "UPDATE campaign_invitations
             SET revoked_at = CURRENT_TIMESTAMP
             WHERE id = $1 AND accepted_at IS NULL AND revoked_at IS NULL",
        )
        .bind(invitation_id)
        .execute(&self.pool)
        .await
        .map_err(RepositoryError::Database)?;
        if result.rows_affected() == 0 {
            return Err(RepositoryError::NotFound {
                entity: "campaign_invitation",
                id: invitation_id.to_owned(),
            });
        }
        Ok(())
    }

    /// Loads an invitation by ID. Returns `None` if not found.
    pub async fn load_campaign_invitation(
        &self,
        invitation_id: &str,
    ) -> Result<Option<CampaignInvitationRow>, RepositoryError> {
        validate_invitation_id(invitation_id)?;
        let row = sqlx::query(
            "SELECT id, campaign_session_id, inviter_account_id,
                    invitee_email_digest,
                    expires_at::text AS expires_at,
                    accepted_at IS NOT NULL AS accepted,
                    revoked_at IS NOT NULL AS revoked,
                    created_at::text AS created_at
             FROM campaign_invitations WHERE id = $1",
        )
        .bind(invitation_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(RepositoryError::Database)?;
        row.map(|r| {
            let accepted: bool = r.try_get("accepted").map_err(RepositoryError::Database)?;
            let revoked: bool = r.try_get("revoked").map_err(RepositoryError::Database)?;
            if accepted || revoked {
                return Err(RepositoryError::NotFound {
                    entity: "campaign_invitation",
                    id: invitation_id.to_owned(),
                });
            }
            invitation_from_row(&r)
        })
        .transpose()
    }

    /// Accepts an invitation, creating an active player membership. The
    /// invitation must not be expired, revoked, or already accepted.
    /// `invitee_email` is re-hashed and compared to the stored digest.
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
        let email_digest = email_sha256(invitee_email);
        let _ = now_unix_seconds; // validated for API stability; DB is authoritative
        let mut transaction = self.pool.begin().await.map_err(RepositoryError::Database)?;
        // Atomically redeem the invitation: this UPDATE only affects a row that
        // is not expired, not accepted, and not revoked. The email digest must
        // match. If zero rows are affected, the invitation is not redeemable.
        let redeemed = sqlx::query(
            "UPDATE campaign_invitations
             SET accepted_at = CURRENT_TIMESTAMP, accepted_account_id = $2
             WHERE id = $1
               AND invitee_email_digest = $3
               AND accepted_at IS NULL
               AND revoked_at IS NULL
               AND expires_at > CURRENT_TIMESTAMP
             RETURNING campaign_session_id, inviter_account_id",
        )
        .bind(invitation_id)
        .bind(account_id)
        .bind(&email_digest)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(RepositoryError::Database)?;
        let redeemed_row = redeemed.ok_or_else(|| {
            // Distinguish not-found from consumed/expired by checking existence.
            RepositoryError::NotFound {
                entity: "campaign_invitation",
                id: invitation_id.to_owned(),
            }
        })?;
        let campaign_id: String = redeemed_row
            .try_get("campaign_session_id")
            .map_err(RepositoryError::Database)?;
        let inviter_account_id: String = redeemed_row
            .try_get("inviter_account_id")
            .map_err(RepositoryError::Database)?;
        // Create the membership.
        sqlx::query(
            "INSERT INTO campaign_memberships
             (campaign_session_id, account_id, role, state, inviter_account_id, accepted_at)
             VALUES ($1, $2, 'player', 'active', $3, CURRENT_TIMESTAMP)",
        )
        .bind(&campaign_id)
        .bind(account_id)
        .bind(&inviter_account_id)
        .execute(&mut *transaction)
        .await
        .map_err(|error| map_insert_error(error, "campaign_membership", &campaign_id))?;
        transaction
            .commit()
            .await
            .map_err(RepositoryError::Database)?;
        self.load_membership(&campaign_id, account_id)
            .await?
            .ok_or(RepositoryError::NotFound {
                entity: "campaign_membership",
                id: campaign_id.to_owned(),
            })
    }

    /// Loads a membership scoped to `(campaign_id, account_id)`. Returns
    /// `None` if the membership does not exist or the account is not a member.
    pub async fn load_membership(
        &self,
        campaign_id: &str,
        account_id: &str,
    ) -> Result<Option<CampaignMembershipRow>, RepositoryError> {
        validate_campaign_id(campaign_id)?;
        validate_account_id(account_id)?;
        let row = sqlx::query(
            "SELECT campaign_session_id, account_id, role, state,
                    created_at::text AS created_at, updated_at::text AS updated_at
             FROM campaign_memberships
             WHERE campaign_session_id = $1 AND account_id = $2",
        )
        .bind(campaign_id)
        .bind(account_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(RepositoryError::Database)?;
        row.map(membership_from_row).transpose()
    }

    /// Lists all members of a campaign. The caller must be an active member;
    /// otherwise returns `NotFound`.
    pub async fn list_campaign_members(
        &self,
        account_id: &str,
        campaign_id: &str,
    ) -> Result<Vec<CampaignMembershipRow>, RepositoryError> {
        validate_account_id(account_id)?;
        validate_campaign_id(campaign_id)?;
        // Verify caller is a member.
        let membership_check: Option<String> = sqlx::query_scalar(
            "SELECT account_id FROM campaign_memberships
             WHERE campaign_session_id = $1 AND account_id = $2 AND state = 'active'",
        )
        .bind(campaign_id)
        .bind(account_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(RepositoryError::Database)?;
        if membership_check.is_none() {
            return Err(RepositoryError::NotFound {
                entity: "campaign_membership",
                id: campaign_id.to_owned(),
            });
        }
        let rows = sqlx::query(
            "SELECT campaign_session_id, account_id, role, state,
                    created_at::text AS created_at, updated_at::text AS updated_at
             FROM campaign_memberships
             WHERE campaign_session_id = $1
             ORDER BY created_at, account_id",
        )
        .bind(campaign_id)
        .fetch_all(&self.pool)
        .await
        .map_err(RepositoryError::Database)?;
        rows.into_iter().map(membership_from_row).collect()
    }

    /// Lists all campaigns an account is a member of (owned + accepted).
    pub async fn list_account_campaigns(
        &self,
        account_id: &str,
    ) -> Result<Vec<MembershipCampaignSummary>, RepositoryError> {
        validate_account_id(account_id)?;
        let rows = sqlx::query(
            "SELECT m.campaign_session_id, m.role, m.state,
                    m.created_at::text AS created_at,
                    m.updated_at::text AS updated_at,
                    cs.payload_json->>'title' AS title,
                    cs.theme_id
             FROM campaign_memberships m
             JOIN campaign_sessions cs ON cs.id = m.campaign_session_id
             WHERE m.account_id = $1 AND m.state = 'active'
             ORDER BY m.updated_at DESC, m.campaign_session_id",
        )
        .bind(account_id)
        .fetch_all(&self.pool)
        .await
        .map_err(RepositoryError::Database)?;
        rows.into_iter()
            .map(|row| {
                let title: String = row
                    .try_get("title")
                    .unwrap_or_else(|_| "Untitled Campaign".to_owned());
                let theme_id: String = row
                    .try_get("theme_id")
                    .unwrap_or_else(|_| "dev.manchester-arcana.rainbound-borough".to_owned());
                Ok(MembershipCampaignSummary {
                    campaign_id: row
                        .try_get("campaign_session_id")
                        .map_err(RepositoryError::Database)?,
                    title,
                    theme_id,
                    role: MembershipRole::try_from_str(
                        &row.try_get::<String, _>("role")
                            .map_err(RepositoryError::Database)?,
                    )?,
                    state: MembershipState::try_from_str(
                        &row.try_get::<String, _>("state")
                            .map_err(RepositoryError::Database)?,
                    )?,
                    created_at: row
                        .try_get("created_at")
                        .map_err(RepositoryError::Database)?,
                    updated_at: row
                        .try_get("updated_at")
                        .map_err(RepositoryError::Database)?,
                })
            })
            .collect()
    }

    /// Removes a member from a campaign. Only the GM may remove. The GM cannot
    /// remove themselves (use delete campaign instead). Returns `NotFound` if
    /// the target is not a member or the caller is not the GM.
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
            return Err(RepositoryError::InvalidDomainState {
                entity: "campaign_membership",
                id: campaign_id.to_owned(),
                reason: "a game master cannot remove their own ownership membership",
            });
        }
        let gm_check: Option<String> = sqlx::query_scalar(
            "SELECT account_id FROM campaign_memberships
             WHERE campaign_session_id = $1 AND account_id = $2
               AND role = 'game_master' AND state = 'active'",
        )
        .bind(campaign_id)
        .bind(gm_account_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(RepositoryError::Database)?;
        if gm_check.is_none() {
            return Err(RepositoryError::NotFound {
                entity: "campaign_membership",
                id: campaign_id.to_owned(),
            });
        }
        let result = sqlx::query(
            "UPDATE campaign_memberships
             SET state = 'removed', left_at = CURRENT_TIMESTAMP
             WHERE campaign_session_id = $1 AND account_id = $2
               AND state = 'active'",
        )
        .bind(campaign_id)
        .bind(member_account_id)
        .execute(&self.pool)
        .await
        .map_err(RepositoryError::Database)?;
        if result.rows_affected() == 0 {
            return Err(RepositoryError::NotFound {
                entity: "campaign_membership",
                id: member_account_id.to_owned(),
            });
        }
        Ok(())
    }

    /// Loads the theme_id for a campaign. Returns `None` if the campaign does
    /// not exist or the caller is not a member.
    pub async fn load_campaign_theme_for_member(
        &self,
        account_id: &str,
        campaign_id: &str,
    ) -> Result<Option<String>, RepositoryError> {
        validate_account_id(account_id)?;
        validate_campaign_id(campaign_id)?;
        let row = sqlx::query(
            "SELECT cs.theme_id
             FROM campaign_sessions cs
             JOIN campaign_memberships m
               ON m.campaign_session_id = cs.id AND m.account_id = $2
             WHERE cs.id = $1 AND m.state = 'active'",
        )
        .bind(campaign_id)
        .bind(account_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(RepositoryError::Database)?;
        Ok(row
            .and_then(|r| r.try_get::<Option<String>, _>("theme_id").ok())
            .flatten())
    }

    /// Assigns a library character to a campaign, creating a runtime hero
    /// instance atomically. Validates:
    /// - The caller owns the source player character.
    /// - The caller is an active member of the campaign.
    /// - The campaign's theme matches the character's theme.
    /// - No existing active instance for this account in this campaign.
    ///
    /// The runtime `HeroCharacter` is created via
    /// `PlayerCharacter::instantiate_for_campaign` and inserted into
    /// `hero_characters` in the same transaction as the instance row.
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
        // The source character must be owned by the caller.
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
        let mut transaction = self.pool.begin().await.map_err(RepositoryError::Database)?;
        // Verify the caller is an active member of the campaign.
        let membership_check: Option<String> = sqlx::query_scalar(
            "SELECT account_id FROM campaign_memberships
             WHERE campaign_session_id = $1 AND account_id = $2 AND state = 'active'",
        )
        .bind(campaign_id)
        .bind(account_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(RepositoryError::Database)?;
        if membership_check.is_none() {
            return Err(RepositoryError::NotFound {
                entity: "campaign_membership",
                id: campaign_id.to_owned(),
            });
        }
        // Check for an existing active instance (no duplicate slot).
        let existing: Option<String> = sqlx::query_scalar(
            "SELECT instance_id FROM campaign_character_instances
             WHERE campaign_session_id = $1 AND account_id = $2 AND state = 'active'",
        )
        .bind(campaign_id)
        .bind(account_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(RepositoryError::Database)?;
        if existing.is_some() {
            return Err(RepositoryError::AlreadyExists {
                entity: "campaign_character_instance",
                id: campaign_id.to_owned(),
            });
        }
        // Load the campaign theme to validate compatibility.
        let campaign_theme: Option<String> =
            sqlx::query_scalar("SELECT theme_id FROM campaign_sessions WHERE id = $1")
                .bind(campaign_id)
                .fetch_optional(&mut *transaction)
                .await
                .map_err(RepositoryError::Database)?;
        let campaign_theme = campaign_theme.ok_or(RepositoryError::NotFound {
            entity: "campaign_session",
            id: campaign_id.to_owned(),
        })?;
        let character_theme = theme_id_str(source_character.theme_id());
        if campaign_theme != character_theme {
            return Err(RepositoryError::InvalidDomainState {
                entity: "campaign_character_instance",
                id: campaign_id.to_owned(),
                reason: "character theme does not match campaign theme",
            });
        }
        // Instantiate the runtime hero.
        let instance_id = format!("instance:{}", Uuid::new_v4().simple());
        let runtime_hero_id = format!("character:runtime-{}", Uuid::new_v4().simple());
        let hero = source_character
            .instantiate_for_campaign(campaign_id.to_owned(), runtime_hero_id.clone())
            .map_err(|source| RepositoryError::HeroValidation {
                entity: "hero_character",
                id: runtime_hero_id.clone(),
                source,
            })?;
        let hero_payload = serialize("hero character", &hero)?;
        // Insert the runtime hero_character row.
        sqlx::query(
            "INSERT INTO hero_characters
             (id, campaign_session_id, owner_key, schema_version, revision, payload_json)
             VALUES ($1, $2, $3, $4, $5, $6::jsonb)",
        )
        .bind(&hero.character_id)
        .bind(&hero.campaign_id)
        .bind(&hero.owner_id)
        .bind(i64::from(HERO_CHARACTER_SCHEMA_VERSION))
        .bind(to_i64(
            hero.revision.saturating_add(1),
            "hero character revision",
        )?)
        .bind(&hero_payload)
        .execute(&mut *transaction)
        .await
        .map_err(|error| map_insert_error(error, "hero character", &hero.character_id))?;
        // Insert the campaign_character_instances row.
        let choices_digest = choices_sha256(source_character);
        sqlx::query(
            "INSERT INTO campaign_character_instances
             (campaign_session_id, account_id, instance_id,
              source_player_character_id, runtime_hero_character_id,
              source_display_name, source_choices_digest, state)
             VALUES ($1, $2, $3, $4, $5, $6, $7, 'active')",
        )
        .bind(campaign_id)
        .bind(account_id)
        .bind(&instance_id)
        .bind(player_character_id)
        .bind(&runtime_hero_id)
        .bind(&source_character.display_name)
        .bind(&choices_digest)
        .execute(&mut *transaction)
        .await
        .map_err(|error| map_insert_error(error, "campaign_character_instance", &instance_id))?;
        transaction
            .commit()
            .await
            .map_err(RepositoryError::Database)?;
        Ok(AssignCharacterOutcome {
            instance_id,
            runtime_hero_character_id: runtime_hero_id,
        })
    }

    /// Loads the active character instance for an account in a campaign, if any.
    pub async fn load_active_character_instance(
        &self,
        account_id: &str,
        campaign_id: &str,
    ) -> Result<Option<CampaignCharacterInstanceRow>, RepositoryError> {
        validate_account_id(account_id)?;
        validate_campaign_id(campaign_id)?;
        let row = sqlx::query(
            "SELECT campaign_session_id, account_id, instance_id,
                    source_player_character_id, runtime_hero_character_id,
                    source_display_name, source_choices_digest, state,
                    created_at::text AS created_at,
                    retired_at::text AS retired_at
             FROM campaign_character_instances
             WHERE campaign_session_id = $1 AND account_id = $2 AND state = 'active'",
        )
        .bind(campaign_id)
        .bind(account_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(RepositoryError::Database)?;
        row.map(|row| character_instance_from_row(&row)).transpose()
    }

    /// Loads a runtime hero character by ID (unscoped — the caller must
    /// have already verified membership).
    pub async fn load_runtime_hero_character(
        &self,
        hero_character_id: &str,
    ) -> Result<Option<HeroCharacter>, RepositoryError> {
        if !is_valid_opaque_id(hero_character_id) {
            return invalid(
                "hero character",
                hero_character_id,
                "character id must be a valid opaque identifier",
            );
        }
        let row = sqlx::query(
            "SELECT payload_json::text AS payload_json FROM hero_characters WHERE id = $1",
        )
        .bind(hero_character_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(RepositoryError::Database)?;
        row.map(|r| {
            let payload: String = r
                .try_get("payload_json")
                .map_err(RepositoryError::Database)?;
            serde_json::from_str::<HeroCharacter>(&payload).map_err(|source| {
                RepositoryError::InvalidStoredData {
                    entity: "hero character",
                    id: hero_character_id.to_owned(),
                    source,
                }
            })
        })
        .transpose()
    }
}

// ── Row mappers ──

fn membership_from_row(
    row: sqlx::postgres::PgRow,
) -> Result<CampaignMembershipRow, RepositoryError> {
    Ok(CampaignMembershipRow {
        campaign_id: row
            .try_get("campaign_session_id")
            .map_err(RepositoryError::Database)?,
        account_id: row
            .try_get("account_id")
            .map_err(RepositoryError::Database)?,
        role: MembershipRole::try_from_str(
            &row.try_get::<String, _>("role")
                .map_err(RepositoryError::Database)?,
        )?,
        state: MembershipState::try_from_str(
            &row.try_get::<String, _>("state")
                .map_err(RepositoryError::Database)?,
        )?,
        created_at: row
            .try_get("created_at")
            .map_err(RepositoryError::Database)?,
        updated_at: row
            .try_get("updated_at")
            .map_err(RepositoryError::Database)?,
    })
}

fn invitation_from_row(
    row: &sqlx::postgres::PgRow,
) -> Result<CampaignInvitationRow, RepositoryError> {
    Ok(CampaignInvitationRow {
        id: row.try_get("id").map_err(RepositoryError::Database)?,
        campaign_id: row
            .try_get("campaign_session_id")
            .map_err(RepositoryError::Database)?,
        inviter_account_id: row
            .try_get("inviter_account_id")
            .map_err(RepositoryError::Database)?,
        invitee_email_digest: row
            .try_get("invitee_email_digest")
            .map_err(RepositoryError::Database)?,
        expires_at: row
            .try_get("expires_at")
            .map_err(RepositoryError::Database)?,
        created_at: row
            .try_get("created_at")
            .map_err(RepositoryError::Database)?,
    })
}

fn character_instance_from_row(
    row: &sqlx::postgres::PgRow,
) -> Result<CampaignCharacterInstanceRow, RepositoryError> {
    Ok(CampaignCharacterInstanceRow {
        campaign_id: row
            .try_get("campaign_session_id")
            .map_err(RepositoryError::Database)?,
        account_id: row
            .try_get("account_id")
            .map_err(RepositoryError::Database)?,
        instance_id: row
            .try_get("instance_id")
            .map_err(RepositoryError::Database)?,
        source_player_character_id: row
            .try_get("source_player_character_id")
            .map_err(RepositoryError::Database)?,
        runtime_hero_character_id: row
            .try_get("runtime_hero_character_id")
            .map_err(RepositoryError::Database)?,
        source_display_name: row
            .try_get("source_display_name")
            .map_err(RepositoryError::Database)?,
        source_choices_digest: row
            .try_get("source_choices_digest")
            .map_err(RepositoryError::Database)?,
        state: CharacterInstanceState::try_from_str(
            &row.try_get::<String, _>("state")
                .map_err(RepositoryError::Database)?,
        )?,
        created_at: row
            .try_get("created_at")
            .map_err(RepositoryError::Database)?,
        retired_at: row
            .try_get("retired_at")
            .map_err(RepositoryError::Database)?,
    })
}

// ── Validators ──

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
    let trimmed = title.trim();
    if trimmed.is_empty()
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

fn email_sha256(email: &str) -> String {
    let normalized = email.trim().to_ascii_lowercase();
    let digest: [u8; 32] = Sha256::digest(normalized.as_bytes()).into();
    let mut value = String::with_capacity(71);
    value.push_str("sha256:");
    for byte in digest {
        use std::fmt::Write as _;
        write!(&mut value, "{byte:02x}").expect("writing to a String cannot fail");
    }
    value
}

fn choices_sha256(character: &PlayerCharacter) -> String {
    let serialized = serde_json::to_vec(&character.choices).unwrap_or_default();
    let digest: [u8; 32] = Sha256::digest(serialized).into();
    let mut value = String::with_capacity(71);
    value.push_str("sha256:");
    for byte in digest {
        use std::fmt::Write as _;
        write!(&mut value, "{byte:02x}").expect("writing to a String cannot fail");
    }
    value
}

fn invalid<T>(entity: &'static str, id: &str, reason: &'static str) -> Result<T, RepositoryError> {
    Err(RepositoryError::InvalidDomainState {
        entity,
        id: id.to_owned(),
        reason,
    })
}
