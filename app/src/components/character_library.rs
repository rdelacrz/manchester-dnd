//! Account-owned character library server functions.
//!
//! These server functions are the browser-facing entry points for listing,
//! creating, viewing, updating, and deleting player characters. All functions
//! derive `account_id` from the authenticated session — the browser never
//! supplies an owner ID.
#![allow(dead_code)]

use leptos::prelude::*;
use serde::{Deserialize, Serialize};

// ── View types ──

/// Safe character summary for the library list. Contains no level, XP, HP,
/// or any campaign-derived runtime state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CharacterSummaryView {
    pub id: String,
    pub display_name: String,
    pub revision: u64,
    pub created_at: String,
    pub updated_at: String,
}

/// Safe character detail view. Contains identity and reusable creation choices
/// only — no campaign_id, level, XP, HP, or sheet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CharacterDetailView {
    pub id: String,
    pub display_name: String,
    pub revision: u64,
    pub created_at: String,
    pub updated_at: String,
    pub theme_id: String,
    pub class_name: String,
    pub ancestry_name: String,
    pub background_name: String,
}

/// Result of listing the authenticated account's characters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", content = "payload", rename_all = "snake_case")]
pub enum CharacterListResponse {
    Success {
        characters: Vec<CharacterSummaryView>,
    },
    Error {
        code: String,
        message: String,
    },
}

/// Result of loading a single character detail.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", content = "payload", rename_all = "snake_case")]
pub enum CharacterDetailResponse {
    Found(CharacterDetailView),
    NotFound,
    Error { code: String, message: String },
}

/// Result of deleting a character.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", content = "payload", rename_all = "snake_case")]
pub enum CharacterDeleteResponse {
    Deleted,
    NotFound,
    Error { code: String, message: String },
}

/// Result of updating a character's display name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", content = "payload", rename_all = "snake_case")]
pub enum CharacterUpdateResponse {
    Updated { revision: u64 },
    NotFound,
    Error { code: String, message: String },
}

impl CharacterListResponse {
    fn internal_error() -> Self {
        Self::Error {
            code: "internal_error".to_owned(),
            message: "Character library is temporarily unavailable.".to_owned(),
        }
    }

    fn authentication_required() -> Self {
        Self::Error {
            code: "authentication_required".to_owned(),
            message: "You must be signed in to view your characters.".to_owned(),
        }
    }
}

impl CharacterDetailResponse {
    fn internal_error() -> Self {
        Self::Error {
            code: "internal_error".to_owned(),
            message: "Character library is temporarily unavailable.".to_owned(),
        }
    }

    fn authentication_required() -> Self {
        Self::Error {
            code: "authentication_required".to_owned(),
            message: "You must be signed in to view characters.".to_owned(),
        }
    }
}

impl CharacterDeleteResponse {
    fn internal_error() -> Self {
        Self::Error {
            code: "internal_error".to_owned(),
            message: "Character library is temporarily unavailable.".to_owned(),
        }
    }

    fn authentication_required() -> Self {
        Self::Error {
            code: "authentication_required".to_owned(),
            message: "You must be signed in to delete a character.".to_owned(),
        }
    }
}

impl CharacterUpdateResponse {
    fn internal_error() -> Self {
        Self::Error {
            code: "internal_error".to_owned(),
            message: "Character library is temporarily unavailable.".to_owned(),
        }
    }

    fn authentication_required() -> Self {
        Self::Error {
            code: "authentication_required".to_owned(),
            message: "You must be signed in to update a character.".to_owned(),
        }
    }
}

// ── Server functions ──

/// Lists all characters owned by the authenticated account.
#[server(ListCharacters)]
pub async fn list_characters() -> Result<CharacterListResponse, ServerFnError> {
    #[cfg(feature = "ssr")]
    {
        let (Some(context), Some(parts)) = (
            use_context::<manchester_dnd_server::ServerContext>(),
            use_context::<axum::http::request::Parts>(),
        ) else {
            return Ok(CharacterListResponse::internal_error());
        };

        let Some(principal) = parts
            .extensions
            .get::<manchester_dnd_server::AccountPrincipal>()
        else {
            return Ok(CharacterListResponse::authentication_required());
        };

        match context
            .application
            .repository()
            .list_player_characters(&principal.account_id)
            .await
        {
            Ok(summaries) => {
                let characters = summaries
                    .into_iter()
                    .map(|s| CharacterSummaryView {
                        id: s.id,
                        display_name: s.display_name,
                        revision: s.revision,
                        created_at: s.created_at,
                        updated_at: s.updated_at,
                    })
                    .collect();
                Ok(CharacterListResponse::Success { characters })
            }
            Err(_) => Ok(CharacterListResponse::internal_error()),
        }
    }

    #[cfg(not(feature = "ssr"))]
    {
        unreachable!("the server-function macro replaces this body in browser builds")
    }
}

/// Loads a single character's detail, scoped to the authenticated account.
#[server(LoadCharacter)]
pub async fn load_character(
    character_id: String,
) -> Result<CharacterDetailResponse, ServerFnError> {
    #[cfg(feature = "ssr")]
    {
        let (Some(context), Some(parts)) = (
            use_context::<manchester_dnd_server::ServerContext>(),
            use_context::<axum::http::request::Parts>(),
        ) else {
            return Ok(CharacterDetailResponse::internal_error());
        };

        let Some(principal) = parts
            .extensions
            .get::<manchester_dnd_server::AccountPrincipal>()
        else {
            return Ok(CharacterDetailResponse::authentication_required());
        };

        match context
            .application
            .repository()
            .load_player_character(&principal.account_id, &character_id)
            .await
        {
            Ok(Some(character)) => {
                let view = CharacterDetailView {
                    id: character.character_id.clone(),
                    display_name: character.display_name.clone(),
                    revision: character.revision,
                    created_at: String::new(),
                    updated_at: String::new(),
                    theme_id: format!("{:?}", character.theme_id()),
                    class_name: format!("{:?}", character.choices.class.class()),
                    ancestry_name: format!("{:?}", character.choices.ancestry),
                    background_name: format!("{:?}", character.choices.background.background),
                };
                Ok(CharacterDetailResponse::Found(view))
            }
            Ok(None) => Ok(CharacterDetailResponse::NotFound),
            Err(_) => Ok(CharacterDetailResponse::internal_error()),
        }
    }

    #[cfg(not(feature = "ssr"))]
    {
        let _ = character_id;
        unreachable!("the server-function macro replaces this body in browser builds")
    }
}

/// Deletes a character, scoped to the authenticated account.
#[server(DeleteCharacter)]
pub async fn delete_character(
    character_id: String,
) -> Result<CharacterDeleteResponse, ServerFnError> {
    #[cfg(feature = "ssr")]
    {
        let (Some(context), Some(parts)) = (
            use_context::<manchester_dnd_server::ServerContext>(),
            use_context::<axum::http::request::Parts>(),
        ) else {
            return Ok(CharacterDeleteResponse::internal_error());
        };

        let Some(principal) = parts
            .extensions
            .get::<manchester_dnd_server::AccountPrincipal>()
        else {
            return Ok(CharacterDeleteResponse::authentication_required());
        };

        match context
            .application
            .repository()
            .delete_player_character(&principal.account_id, &character_id)
            .await
        {
            Ok(true) => Ok(CharacterDeleteResponse::Deleted),
            Ok(false) => Ok(CharacterDeleteResponse::NotFound),
            Err(_) => Ok(CharacterDeleteResponse::internal_error()),
        }
    }

    #[cfg(not(feature = "ssr"))]
    {
        let _ = character_id;
        unreachable!("the server-function macro replaces this body in browser builds")
    }
}

/// Updates a character's display name, scoped to the authenticated account.
#[server(UpdateCharacterName)]
pub async fn update_character_name(
    character_id: String,
    expected_revision: u64,
    new_display_name: String,
) -> Result<CharacterUpdateResponse, ServerFnError> {
    #[cfg(feature = "ssr")]
    {
        let (Some(context), Some(parts)) = (
            use_context::<manchester_dnd_server::ServerContext>(),
            use_context::<axum::http::request::Parts>(),
        ) else {
            return Ok(CharacterUpdateResponse::internal_error());
        };

        let Some(principal) = parts
            .extensions
            .get::<manchester_dnd_server::AccountPrincipal>()
        else {
            return Ok(CharacterUpdateResponse::authentication_required());
        };

        match context
            .application
            .repository()
            .update_player_character_display_name(
                &principal.account_id,
                &character_id,
                expected_revision,
                &new_display_name,
            )
            .await
        {
            Ok(new_revision) => Ok(CharacterUpdateResponse::Updated {
                revision: new_revision,
            }),
            Err(manchester_dnd_server::RepositoryError::NotFound { .. }) => {
                Ok(CharacterUpdateResponse::NotFound)
            }
            Err(_) => Ok(CharacterUpdateResponse::internal_error()),
        }
    }

    #[cfg(not(feature = "ssr"))]
    {
        let _ = (character_id, expected_revision, new_display_name);
        unreachable!("the server-function macro replaces this body in browser builds")
    }
}

#[cfg(all(test, feature = "ssr"))]
mod tests {
    use super::*;

    #[test]
    fn character_summary_view_denies_unknown_fields() {
        let json = r#"{"id":"character:test","display_name":"Test","revision":0,"created_at":"","updated_at":"","extra":true}"#;
        assert!(serde_json::from_str::<CharacterSummaryView>(json).is_err());
    }

    #[test]
    fn character_detail_view_denies_unknown_fields() {
        let json = r#"{"id":"character:test","display_name":"Test","revision":0,"created_at":"","updated_at":"","theme_id":"test","class_name":"Fighter","ancestry_name":"Human","background_name":"Soldier","level":5}"#;
        assert!(serde_json::from_str::<CharacterDetailView>(json).is_err());
    }

    #[test]
    fn character_list_response_serializes_correctly() {
        let response = CharacterListResponse::Success {
            characters: vec![CharacterSummaryView {
                id: "character:test".to_owned(),
                display_name: "Test Hero".to_owned(),
                revision: 0,
                created_at: "2025-01-01".to_owned(),
                updated_at: "2025-01-01".to_owned(),
            }],
        };
        let json = serde_json::to_string(&response).unwrap();
        assert!(json.contains("\"status\":\"success\""));
        assert!(json.contains("Test Hero"));
        // No level, XP, or HP fields.
        assert!(!json.contains("level"));
        assert!(!json.contains("experience"));
        assert!(!json.contains("hit_points"));
    }
}
