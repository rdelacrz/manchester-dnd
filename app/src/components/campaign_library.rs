use leptos::prelude::*;
use serde::{Deserialize, Serialize};

#[allow(dead_code)]
// ── View types ──
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum CampaignLibraryResponse {
    Ready {
        owned: Vec<CampaignSummaryView>,
        memberships: Vec<CampaignSummaryView>,
        invitations: Vec<InvitationView>,
    },
    AuthenticationRequired,
    Error {
        code: String,
        message: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct CampaignSummaryView {
    pub campaign_id: String,
    pub title: String,
    pub theme_id: String,
    pub role: String,
    pub state: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct InvitationView {
    pub invitation_id: String,
    pub campaign_id: String,
    pub campaign_title: String,
    pub role: String,
    pub expires_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct CreateCampaignInput {
    pub title: String,
    pub theme_id: String,
}

#[allow(dead_code)]
impl CampaignLibraryResponse {
    pub fn authentication_required() -> Self {
        Self::AuthenticationRequired
    }

    pub fn error(code: &str, message: &str) -> Self {
        Self::Error {
            code: code.to_owned(),
            message: message.to_owned(),
        }
    }

    pub fn internal_error() -> Self {
        Self::error("internal_error", "Campaigns are temporarily unavailable.")
    }
}

// ── Server functions ──

/// Lists all campaigns the authenticated account owns or has accepted
/// membership in, plus pending invitations.
#[server(ListAccountCampaigns)]
pub async fn list_account_campaigns() -> Result<CampaignLibraryResponse, ServerFnError> {
    #[cfg(feature = "ssr")]
    {
        let (Some(context), Some(parts)) = (
            use_context::<manchester_dnd_server::ServerContext>(),
            use_context::<axum::http::request::Parts>(),
        ) else {
            return Ok(CampaignLibraryResponse::internal_error());
        };

        let Some(principal) = parts
            .extensions
            .get::<manchester_dnd_server::AccountPrincipal>()
        else {
            return Ok(CampaignLibraryResponse::authentication_required());
        };

        let repo = context.application.repository();

        match repo.list_account_campaigns(&principal.account_id).await {
            Ok(campaigns) => {
                let owned: Vec<_> = campaigns
                    .iter()
                    .filter(|c| c.role == manchester_dnd_server::MembershipRole::GameMaster)
                    .map(summary_view)
                    .collect();
                let memberships: Vec<_> = campaigns
                    .iter()
                    .filter(|c| c.role == manchester_dnd_server::MembershipRole::Player)
                    .map(summary_view)
                    .collect();

                Ok(CampaignLibraryResponse::Ready {
                    owned,
                    memberships,
                    invitations: Vec::new(),
                })
            }
            Err(_) => Ok(CampaignLibraryResponse::internal_error()),
        }
    }
    #[cfg(not(feature = "ssr"))]
    unreachable!("the server-function macro replaces this body in browser builds")
}

/// Creates a new campaign with the authenticated account as game master.
#[server(CreateAccountCampaign)]
pub async fn create_campaign(
    input: CreateCampaignInput,
) -> Result<CampaignLibraryResponse, ServerFnError> {
    #[cfg(feature = "ssr")]
    {
        let (Some(context), Some(parts)) = (
            use_context::<manchester_dnd_server::ServerContext>(),
            use_context::<axum::http::request::Parts>(),
        ) else {
            return Ok(CampaignLibraryResponse::internal_error());
        };

        let Some(principal) = parts
            .extensions
            .get::<manchester_dnd_server::AccountPrincipal>()
        else {
            return Ok(CampaignLibraryResponse::authentication_required());
        };

        let title = input.title.trim();
        if title.is_empty() || title.chars().count() > 200 {
            return Ok(CampaignLibraryResponse::error(
                "invalid_input",
                "Campaign title must be 1–200 characters.",
            ));
        }

        let repo = context.application.repository();

        match repo
            .create_campaign_with_owner(&principal.account_id, title, &input.theme_id)
            .await
        {
            Ok(outcome) => Ok(CampaignLibraryResponse::Ready {
                owned: vec![CampaignSummaryView {
                    campaign_id: outcome.campaign_id,
                    title: title.to_owned(),
                    theme_id: input.theme_id,
                    role: "game_master".to_owned(),
                    state: "active".to_owned(),
                    created_at: String::new(),
                }],
                memberships: Vec::new(),
                invitations: Vec::new(),
            }),
            Err(_) => Ok(CampaignLibraryResponse::internal_error()),
        }
    }
    #[cfg(not(feature = "ssr"))]
    unreachable!("the server-function macro replaces this body in browser builds")
}

#[cfg(feature = "ssr")]
fn summary_view(c: &manchester_dnd_server::MembershipCampaignSummary) -> CampaignSummaryView {
    CampaignSummaryView {
        campaign_id: c.campaign_id.clone(),
        title: c.title.clone(),
        theme_id: c.theme_id.clone(),
        role: match c.role {
            manchester_dnd_server::MembershipRole::GameMaster => "game_master".to_owned(),
            manchester_dnd_server::MembershipRole::Player => "player".to_owned(),
        },
        state: match c.state {
            manchester_dnd_server::MembershipState::Invited => "invited".to_owned(),
            manchester_dnd_server::MembershipState::Active => "active".to_owned(),
            manchester_dnd_server::MembershipState::Left => "left".to_owned(),
            manchester_dnd_server::MembershipState::Removed => "removed".to_owned(),
        },
        created_at: c.created_at.clone(),
    }
}

// ── Tests ──

#[cfg(all(test, feature = "ssr"))]
mod tests {
    use super::*;

    #[test]
    fn authentication_required_response_has_safe_fields() {
        let resp = CampaignLibraryResponse::authentication_required();
        match resp {
            CampaignLibraryResponse::AuthenticationRequired => {}
            _ => panic!("expected AuthenticationRequired variant"),
        }
    }

    #[test]
    fn error_response_does_not_leak_internals() {
        let resp = CampaignLibraryResponse::internal_error();
        match resp {
            CampaignLibraryResponse::Error { message, .. } => {
                assert!(!message.to_lowercase().contains("sql"));
                assert!(!message.to_lowercase().contains("database"));
                assert!(!message.to_lowercase().contains("query"));
            }
            _ => panic!("expected Error variant"),
        }
    }

    #[test]
    fn create_campaign_input_rejects_empty_and_oversized_titles() {
        let input = CreateCampaignInput {
            title: String::new(),
            theme_id: "emberline".to_owned(),
        };
        assert!(input.title.trim().is_empty());

        let long_title = "x".repeat(201);
        let input2 = CreateCampaignInput {
            title: long_title,
            theme_id: "emberline".to_owned(),
        };
        assert!(input2.title.chars().count() > 200);
    }
}
