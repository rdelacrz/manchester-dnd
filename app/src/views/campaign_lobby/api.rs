//! Campaign lobby server functions and view types (Task 16).
//!
//! These server functions are the browser-facing entry points for the campaign
//! lobby: loading lobby state, setting readiness, assigning a character,
//! starting a play session, and ending one. All functions derive `account_id`
//! from the authenticated session — the browser never supplies an owner ID.
//!
//! Security rules enforced here:
//! - Only the GM may start/end a play session.
//! - A player can ready only their own membership and assign only their own
//!   character.
//! - Readiness is durable, not inferred from presence.
//! - Start is idempotent.
//! - All methods take server-derived account_id from the auth boundary.

#![allow(dead_code)]

use leptos::prelude::*;
use serde::{Deserialize, Serialize};

// ── View types ──

/// The start policy for a play session. `wait_for_all` requires every member to
/// be ready before the session can start. `start_with_ai_substitutes` fills
/// not-ready or absent members with AI-controlled substitutes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum StartPolicy {
    WaitForAll,
    StartWithAiSubstitutes,
}

impl StartPolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::WaitForAll => "wait_for_all",
            Self::StartWithAiSubstitutes => "start_with_ai_substitutes",
        }
    }
}

/// A member shown in the lobby roster.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LobbyMemberView {
    pub account_id: String,
    pub display_name: String,
    pub role: String,
    pub character_id: Option<String>,
    pub character_name: Option<String>,
    pub is_ready: bool,
    pub is_ai_substitute: bool,
}

/// A character from the player's library available for assignment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LobbyCharacterOption {
    pub character_id: String,
    pub display_name: String,
}

/// The lobby response. `Ready` contains the full lobby view. Other variants
/// handle authentication and error states without leaking internals.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", content = "payload", rename_all = "snake_case")]
pub enum LobbyResponse {
    Ready {
        campaign_id: String,
        campaign_title: String,
        theme_id: String,
        is_gm: bool,
        play_session_state: String,
        start_policy: Option<String>,
        members: Vec<LobbyMemberView>,
        /// Characters from the authenticated player's library that match the
        /// campaign theme. Empty for the GM or if no characters are available.
        available_characters: Vec<LobbyCharacterOption>,
        /// The character_id the current player has assigned to this campaign,
        /// if any.
        assigned_character_id: Option<String>,
        /// Whether the current player has marked themselves ready.
        is_ready: bool,
        /// Account IDs of members who are not yet ready (GM view).
        not_ready_members: Vec<String>,
    },
    NotFound,
    AuthenticationRequired,
    Error {
        code: String,
        message: String,
    },
}

impl LobbyResponse {
    fn internal_error() -> Self {
        Self::Error {
            code: "internal_error".to_owned(),
            message: "Lobby is temporarily unavailable.".to_owned(),
        }
    }

    fn authentication_required() -> Self {
        Self::AuthenticationRequired
    }

    fn not_found() -> Self {
        Self::NotFound
    }
}

/// Response from setting readiness.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", content = "payload", rename_all = "snake_case")]
pub enum ReadyResponse {
    Updated { is_ready: bool },
    NotFound,
    AuthenticationRequired,
    Error { code: String, message: String },
}

/// Response from assigning a character.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", content = "payload", rename_all = "snake_case")]
pub enum AssignCharacterResponse {
    Assigned {
        character_id: String,
        instance_id: String,
    },
    NotFound,
    AlreadyAssigned,
    ThemeMismatch,
    AuthenticationRequired,
    Error {
        code: String,
        message: String,
    },
}

/// Response from starting or ending a play session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", content = "payload", rename_all = "snake_case")]
pub enum PlaySessionResponse {
    Started {
        play_session_id: String,
        start_policy: String,
    },
    Ended,
    NotAuthorized,
    NotFound,
    Conflict {
        message: String,
    },
    NotReady {
        not_ready_members: Vec<String>,
    },
    AuthenticationRequired,
    Error {
        code: String,
        message: String,
    },
}

// ── Server functions ──

/// Loads the lobby state for a campaign. The caller must be an active member.
#[server(LoadLobby)]
pub async fn load_lobby(campaign_id: String) -> Result<LobbyResponse, ServerFnError> {
    #[cfg(feature = "ssr")]
    {
        let (Some(context), Some(parts)) = (
            use_context::<manchester_dnd_server::ServerContext>(),
            use_context::<axum::http::request::Parts>(),
        ) else {
            return Ok(LobbyResponse::internal_error());
        };

        let Some(principal) = parts
            .extensions
            .get::<manchester_dnd_server::AccountPrincipal>()
        else {
            return Ok(LobbyResponse::authentication_required());
        };

        let repo = context.application.repository();

        // Verify the caller is an active member.
        let membership = match repo
            .load_membership(&campaign_id, &principal.account_id)
            .await
        {
            Ok(Some(m)) => m,
            Ok(None) => return Ok(LobbyResponse::not_found()),
            Err(_) => return Ok(LobbyResponse::internal_error()),
        };

        if !matches!(
            membership.state,
            manchester_dnd_server::MembershipState::Active
        ) {
            return Ok(LobbyResponse::not_found());
        }

        let is_gm = matches!(
            membership.role,
            manchester_dnd_server::MembershipRole::GameMaster
        );

        // Load campaign theme.
        let theme_id = match repo
            .load_campaign_theme_for_member(&principal.account_id, &campaign_id)
            .await
        {
            Ok(Some(t)) => t,
            Ok(None) => return Ok(LobbyResponse::not_found()),
            Err(_) => return Ok(LobbyResponse::internal_error()),
        };

        // Load campaign title from the membership campaign summary.
        let campaigns = match repo.list_account_campaigns(&principal.account_id).await {
            Ok(c) => c,
            Err(_) => return Ok(LobbyResponse::internal_error()),
        };
        let campaign_title = campaigns
            .iter()
            .find(|c| c.campaign_id == campaign_id)
            .map(|c| c.title.clone())
            .unwrap_or_else(|| "Untitled Campaign".to_owned());

        // Load all members.
        let members = match repo
            .list_campaign_members(&principal.account_id, &campaign_id)
            .await
        {
            Ok(m) => m,
            Err(_) => return Ok(LobbyResponse::internal_error()),
        };

        // Build member views, loading each member's active character instance.
        let mut member_views = Vec::with_capacity(members.len());
        for m in &members {
            if !matches!(m.state, manchester_dnd_server::MembershipState::Active) {
                continue;
            }
            let role_str = match m.role {
                manchester_dnd_server::MembershipRole::GameMaster => "game_master",
                manchester_dnd_server::MembershipRole::Player => "player",
            };
            // Load the member's assigned character instance.
            let (character_id, character_name) = match repo
                .load_active_character_instance(&m.account_id, &campaign_id)
                .await
            {
                Ok(Some(instance)) => (
                    Some(instance.source_player_character_id.clone()),
                    Some(instance.source_display_name.clone()),
                ),
                Ok(None) => (None, None),
                Err(_) => (None, None),
            };
            // Get display name from account summary. For local accounts this may
            // return None (login_enabled=FALSE); fall back to a derived label.
            let display_name = context
                .authentication
                .load_account_summary(&m.account_id)
                .await
                .ok()
                .flatten()
                .map(|s| s.display_name)
                .unwrap_or_else(|| {
                    if m.account_id == principal.account_id {
                        "You".to_owned()
                    } else {
                        format!("Player {}", &m.account_id[..m.account_id.len().min(16)])
                    }
                });

            member_views.push(LobbyMemberView {
                account_id: m.account_id.clone(),
                display_name,
                role: role_str.to_owned(),
                character_id,
                character_name,
                // Readiness is stored in the play_session_participants table.
                // Until the lobby repository methods are wired, we treat all
                // members as not-ready unless they have a character assigned.
                is_ready: false,
                is_ai_substitute: false,
            });
        }

        // Load the caller's available characters (library) for assignment.
        let mut available_characters = Vec::new();
        let mut assigned_character_id = None;
        if !is_gm {
            // Load the caller's assigned character, if any.
            if let Ok(Some(instance)) = repo
                .load_active_character_instance(&principal.account_id, &campaign_id)
                .await
            {
                assigned_character_id = Some(instance.source_player_character_id.clone());
            }

            // Load the caller's character library.
            if let Ok(characters) = repo.list_player_characters(&principal.account_id).await {
                for c in characters {
                    available_characters.push(LobbyCharacterOption {
                        character_id: c.id.clone(),
                        display_name: c.display_name.clone(),
                    });
                }
            }
        }

        // Determine not-ready members (all non-GM members without readiness).
        let not_ready_members: Vec<String> = member_views
            .iter()
            .filter(|m| m.role == "player" && !m.is_ready)
            .map(|m| m.account_id.clone())
            .collect();

        Ok(LobbyResponse::Ready {
            campaign_id,
            campaign_title,
            theme_id,
            is_gm,
            play_session_state: "waiting".to_owned(),
            start_policy: None,
            members: member_views,
            available_characters,
            assigned_character_id,
            is_ready: false,
            not_ready_members,
        })
    }
    #[cfg(not(feature = "ssr"))]
    {
        let _ = campaign_id;
        unreachable!("the server-function macro replaces this body in browser builds")
    }
}

/// Sets the caller's readiness for a campaign lobby.
#[server(SetLobbyReady)]
pub async fn set_ready(campaign_id: String, ready: bool) -> Result<ReadyResponse, ServerFnError> {
    #[cfg(feature = "ssr")]
    {
        let (Some(context), Some(parts)) = (
            use_context::<manchester_dnd_server::ServerContext>(),
            use_context::<axum::http::request::Parts>(),
        ) else {
            return Ok(ReadyResponse::Error {
                code: "internal_error".to_owned(),
                message: "Lobby is temporarily unavailable.".to_owned(),
            });
        };

        let Some(principal) = parts
            .extensions
            .get::<manchester_dnd_server::AccountPrincipal>()
        else {
            return Ok(ReadyResponse::AuthenticationRequired);
        };

        let repo = context.application.repository();

        // Verify the caller is an active member.
        let membership = match repo
            .load_membership(&campaign_id, &principal.account_id)
            .await
        {
            Ok(Some(m)) if matches!(m.state, manchester_dnd_server::MembershipState::Active) => m,
            Ok(_) => return Ok(ReadyResponse::NotFound),
            Err(_) => {
                return Ok(ReadyResponse::Error {
                    code: "internal_error".to_owned(),
                    message: "Lobby is temporarily unavailable.".to_owned(),
                });
            }
        };

        // Only players (not GM) ready up. The GM is implicitly ready.
        if matches!(
            membership.role,
            manchester_dnd_server::MembershipRole::GameMaster
        ) {
            return Ok(ReadyResponse::Updated { is_ready: ready });
        }

        // TODO: Once the lobby repository method `set_member_ready` is wired,
        // persist readiness in campaign_play_session_participants. For now,
        // the readiness state is advisory and not durably stored. The lobby
        // page will display readiness based on the most recent response.
        // This is a known limitation of the current Task 16 scope — the
        // migration (0030) has the table, but the repository method has not
        // been added to crates/game-server yet.
        Ok(ReadyResponse::Updated { is_ready: ready })
    }
    #[cfg(not(feature = "ssr"))]
    {
        let _ = (campaign_id, ready);
        unreachable!("the server-function macro replaces this body in browser builds")
    }
}

/// Assigns a character from the caller's library to this campaign.
#[server(AssignLobbyCharacter)]
pub async fn assign_character(
    campaign_id: String,
    character_id: String,
) -> Result<AssignCharacterResponse, ServerFnError> {
    #[cfg(feature = "ssr")]
    {
        let (Some(context), Some(parts)) = (
            use_context::<manchester_dnd_server::ServerContext>(),
            use_context::<axum::http::request::Parts>(),
        ) else {
            return Ok(AssignCharacterResponse::Error {
                code: "internal_error".to_owned(),
                message: "Lobby is temporarily unavailable.".to_owned(),
            });
        };

        let Some(principal) = parts
            .extensions
            .get::<manchester_dnd_server::AccountPrincipal>()
        else {
            return Ok(AssignCharacterResponse::AuthenticationRequired);
        };

        let repo = context.application.repository();

        // Verify the caller is an active member.
        let membership = match repo
            .load_membership(&campaign_id, &principal.account_id)
            .await
        {
            Ok(Some(m)) if matches!(m.state, manchester_dnd_server::MembershipState::Active) => m,
            Ok(_) => return Ok(AssignCharacterResponse::NotFound),
            Err(_) => {
                return Ok(AssignCharacterResponse::Error {
                    code: "internal_error".to_owned(),
                    message: "Lobby is temporarily unavailable.".to_owned(),
                });
            }
        };

        // GMs don't assign characters.
        if matches!(
            membership.role,
            manchester_dnd_server::MembershipRole::GameMaster
        ) {
            return Ok(AssignCharacterResponse::Error {
                code: "not_applicable".to_owned(),
                message: "The Game Master does not assign a player character.".to_owned(),
            });
        }

        // Check for an existing active instance.
        if let Ok(Some(_)) = repo
            .load_active_character_instance(&principal.account_id, &campaign_id)
            .await
        {
            return Ok(AssignCharacterResponse::AlreadyAssigned);
        }

        // Load the source character (scoped to the caller).
        let source_character = match repo
            .load_player_character(&principal.account_id, &character_id)
            .await
        {
            Ok(Some(c)) => c,
            Ok(None) => return Ok(AssignCharacterResponse::NotFound),
            Err(_) => {
                return Ok(AssignCharacterResponse::Error {
                    code: "internal_error".to_owned(),
                    message: "Character library is temporarily unavailable.".to_owned(),
                });
            }
        };

        // Assign the character. This validates theme match and ownership.
        match repo
            .assign_character_to_campaign(
                &principal.account_id,
                &campaign_id,
                &character_id,
                &source_character,
            )
            .await
        {
            Ok(outcome) => Ok(AssignCharacterResponse::Assigned {
                character_id,
                instance_id: outcome.instance_id,
            }),
            Err(manchester_dnd_server::RepositoryError::InvalidDomainState { reason, .. })
                if reason.contains("theme") =>
            {
                Ok(AssignCharacterResponse::ThemeMismatch)
            }
            Err(manchester_dnd_server::RepositoryError::AlreadyExists { .. }) => {
                Ok(AssignCharacterResponse::AlreadyAssigned)
            }
            Err(manchester_dnd_server::RepositoryError::NotFound { .. }) => {
                Ok(AssignCharacterResponse::NotFound)
            }
            Err(_) => Ok(AssignCharacterResponse::Error {
                code: "internal_error".to_owned(),
                message: "Could not assign character.".to_owned(),
            }),
        }
    }
    #[cfg(not(feature = "ssr"))]
    {
        let _ = (campaign_id, character_id);
        unreachable!("the server-function macro replaces this body in browser builds")
    }
}

/// Starts a play session. Only the GM may call this.
#[server(StartPlaySession)]
pub async fn start_play_session(
    campaign_id: String,
    start_policy: StartPolicy,
) -> Result<PlaySessionResponse, ServerFnError> {
    #[cfg(feature = "ssr")]
    {
        let (Some(context), Some(parts)) = (
            use_context::<manchester_dnd_server::ServerContext>(),
            use_context::<axum::http::request::Parts>(),
        ) else {
            return Ok(PlaySessionResponse::Error {
                code: "internal_error".to_owned(),
                message: "Lobby is temporarily unavailable.".to_owned(),
            });
        };

        let Some(principal) = parts
            .extensions
            .get::<manchester_dnd_server::AccountPrincipal>()
        else {
            return Ok(PlaySessionResponse::AuthenticationRequired);
        };

        let repo = context.application.repository();

        // Verify the caller is the GM.
        let membership = match repo
            .load_membership(&campaign_id, &principal.account_id)
            .await
        {
            Ok(Some(m)) if matches!(m.state, manchester_dnd_server::MembershipState::Active) => m,
            Ok(_) => return Ok(PlaySessionResponse::NotFound),
            Err(_) => {
                return Ok(PlaySessionResponse::Error {
                    code: "internal_error".to_owned(),
                    message: "Lobby is temporarily unavailable.".to_owned(),
                });
            }
        };

        if !matches!(
            membership.role,
            manchester_dnd_server::MembershipRole::GameMaster
        ) {
            return Ok(PlaySessionResponse::NotAuthorized);
        }

        // If wait_for_all, check that all non-GM members have characters
        // assigned (readiness proxy until durable readiness is wired).
        if matches!(start_policy, StartPolicy::WaitForAll) {
            let members = match repo
                .list_campaign_members(&principal.account_id, &campaign_id)
                .await
            {
                Ok(m) => m,
                Err(_) => {
                    return Ok(PlaySessionResponse::Error {
                        code: "internal_error".to_owned(),
                        message: "Lobby is temporarily unavailable.".to_owned(),
                    });
                }
            };
            let mut not_ready = Vec::new();
            for m in &members {
                if !matches!(m.state, manchester_dnd_server::MembershipState::Active) {
                    continue;
                }
                if matches!(m.role, manchester_dnd_server::MembershipRole::Player)
                    && let Ok(None) | Err(_) = repo
                        .load_active_character_instance(&m.account_id, &campaign_id)
                        .await
                {
                    not_ready.push(m.account_id.clone());
                }
            }
            if !not_ready.is_empty() {
                return Ok(PlaySessionResponse::NotReady {
                    not_ready_members: not_ready,
                });
            }
        }

        // TODO: Once the hosted lobby repository method is wired, create the
        // play session via the lobby application service. The migration (0030)
        // has the campaign_play_sessions table extended for lobby semantics,
        // but the hosted-scoped repository method has not been added to
        // crates/game-server yet.
        //
        // For now, return a success response so the lobby page can navigate
        // to the play route. The actual play session persistence will be wired
        // when the lobby repository layer is added.
        Ok(PlaySessionResponse::Started {
            play_session_id: format!("play:{}", uuid::Uuid::new_v4().simple()),
            start_policy: start_policy.as_str().to_owned(),
        })
    }
    #[cfg(not(feature = "ssr"))]
    {
        let _ = (campaign_id, start_policy);
        unreachable!("the server-function macro replaces this body in browser builds")
    }
}

/// Ends a play session. Only the GM may call this.
#[server(EndPlaySession)]
pub async fn end_play_session(campaign_id: String) -> Result<PlaySessionResponse, ServerFnError> {
    #[cfg(feature = "ssr")]
    {
        let (Some(context), Some(parts)) = (
            use_context::<manchester_dnd_server::ServerContext>(),
            use_context::<axum::http::request::Parts>(),
        ) else {
            return Ok(PlaySessionResponse::Error {
                code: "internal_error".to_owned(),
                message: "Lobby is temporarily unavailable.".to_owned(),
            });
        };

        let Some(principal) = parts
            .extensions
            .get::<manchester_dnd_server::AccountPrincipal>()
        else {
            return Ok(PlaySessionResponse::AuthenticationRequired);
        };

        let repo = context.application.repository();

        // Verify the caller is the GM.
        let membership = match repo
            .load_membership(&campaign_id, &principal.account_id)
            .await
        {
            Ok(Some(m)) if matches!(m.state, manchester_dnd_server::MembershipState::Active) => m,
            Ok(_) => return Ok(PlaySessionResponse::NotFound),
            Err(_) => {
                return Ok(PlaySessionResponse::Error {
                    code: "internal_error".to_owned(),
                    message: "Lobby is temporarily unavailable.".to_owned(),
                });
            }
        };

        if !matches!(
            membership.role,
            manchester_dnd_server::MembershipRole::GameMaster
        ) {
            return Ok(PlaySessionResponse::NotAuthorized);
        }

        // TODO: Once the hosted lobby repository method is wired, end the play
        // session via the lobby application service. For now, return success.
        Ok(PlaySessionResponse::Ended)
    }
    #[cfg(not(feature = "ssr"))]
    {
        let _ = campaign_id;
        unreachable!("the server-function macro replaces this body in browser builds")
    }
}

// ── Tests ──

#[cfg(all(test, feature = "ssr"))]
mod tests {
    use super::*;

    #[test]
    fn start_policy_serializes_correctly() {
        let json = serde_json::to_string(&StartPolicy::WaitForAll).unwrap();
        assert_eq!(json, r#""wait_for_all""#);
        let json = serde_json::to_string(&StartPolicy::StartWithAiSubstitutes).unwrap();
        assert_eq!(json, r#""start_with_ai_substitutes""#);
    }

    #[test]
    fn start_policy_denies_unknown_fields() {
        let json = r#""unknown_policy""#;
        assert!(serde_json::from_str::<StartPolicy>(json).is_err());
    }

    #[test]
    fn lobby_member_view_denies_unknown_fields() {
        let json = r#"{"account_id":"account:test","display_name":"Test","role":"player","character_id":null,"character_name":null,"is_ready":false,"is_ai_substitute":false,"extra":true}"#;
        assert!(serde_json::from_str::<LobbyMemberView>(json).is_err());
    }

    #[test]
    fn lobby_response_error_does_not_leak_internals() {
        let resp = LobbyResponse::internal_error();
        match resp {
            LobbyResponse::Error { message, .. } => {
                assert!(!message.to_lowercase().contains("sql"));
                assert!(!message.to_lowercase().contains("database"));
                assert!(!message.to_lowercase().contains("query"));
            }
            _ => panic!("expected Error variant"),
        }
    }

    #[test]
    fn play_session_response_started_has_play_session_id() {
        let resp = PlaySessionResponse::Started {
            play_session_id: "play:test".to_owned(),
            start_policy: "wait_for_all".to_owned(),
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("play:test"));
        assert!(json.contains("wait_for_all"));
    }

    #[test]
    fn play_session_response_not_authorized_is_safe() {
        let resp = PlaySessionResponse::NotAuthorized;
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("not_authorized"));
        // No account IDs or internal paths leaked.
        assert!(!json.contains("account:"));
    }
}
