use leptos::prelude::*;
use leptos_meta::Title;

pub(crate) mod library;

use self::library::{
    CampaignLibraryResponse, CampaignSummaryView, InvitationView, list_account_campaigns,
};
use crate::components::protected_layout::ProtectedLayout;

#[component]
pub fn CampaignsPage() -> impl IntoView {
    let campaigns = Resource::new(
        move || (),
        move |_| async { list_account_campaigns().await },
    );

    view! {
        <Title text="Campaigns · Manchester Arcana"/>
        <ProtectedLayout>
            <CampaignsContent campaigns=campaigns />
        </ProtectedLayout>
    }
}

#[component]
fn CampaignsContent(
    campaigns: Resource<Result<CampaignLibraryResponse, ServerFnError>>,
) -> impl IntoView {
    view! {
        <section class="protected-page campaigns-page" aria-labelledby="campaigns-heading">
            <div class="characters-header">
                <div>
                    <p class="eyebrow">"YOUR CAMPAIGNS"</p>
                    <h1 id="campaigns-heading">"Campaigns"</h1>
                </div>
                <a class="primary-button" href="/campaigns/new" data-testid="create-campaign-link">
                    "Create a campaign"
                </a>
            </div>

            <Suspense fallback=|| {
                view! {
                    <div class="auth-loading" role="status" aria-live="polite">
                        "Loading campaigns…"
                    </div>
                }
            }>
                {move || {
                    campaigns.get().map(|result| {
                        match result {
                            Ok(CampaignLibraryResponse::Ready { owned, memberships, invitations }) => {
                                let empty = owned.is_empty() && memberships.is_empty() && invitations.is_empty();
                                if empty {
                                    view! {
                                        <div class="protected-placeholder" data-testid="campaigns-empty">
                                            <p>"No campaigns yet. Create one to get started."</p>
                                        </div>
                                    }.into_any()
                                } else {
                                    let owned_view = if !owned.is_empty() {
                                        Some(view! {
                                            <div class="campaign-section" data-testid="owned-campaigns">
                                                <h2>"Campaigns you run"</h2>
                                                {owned.iter().map(|c| campaign_card(c, true)).collect::<Vec<_>>()}
                                            </div>
                                        })
                                    } else {
                                        None
                                    };
                                    let member_view = if !memberships.is_empty() {
                                        Some(view! {
                                            <div class="campaign-section" data-testid="member-campaigns">
                                                <h2>"Campaigns you play in"</h2>
                                                {memberships.iter().map(|c| campaign_card(c, false)).collect::<Vec<_>>()}
                                            </div>
                                        })
                                    } else {
                                        None
                                    };
                                    let invite_view = if !invitations.is_empty() {
                                        Some(view! {
                                            <div class="campaign-section" data-testid="pending-invitations">
                                                <h2>"Pending invitations"</h2>
                                                {invitations.iter().map(invitation_card).collect::<Vec<_>>()}
                                            </div>
                                        })
                                    } else {
                                        None
                                    };
                                    view! {
                                        <div class="campaigns-list">
                                            {owned_view}
                                            {member_view}
                                            {invite_view}
                                        </div>
                                    }.into_any()
                                }
                            }
                            Ok(CampaignLibraryResponse::AuthenticationRequired) => {
                                view! {
                                    <p class="auth-error" role="alert">"Please log in to view your campaigns."</p>
                                }.into_any()
                            }
                            Ok(CampaignLibraryResponse::Error { message, .. }) => {
                                view! {
                                    <p class="auth-error" role="alert">{message}</p>
                                }.into_any()
                            }
                            Err(_) => {
                                view! {
                                    <p class="auth-error" role="alert">"Campaigns are temporarily unavailable."</p>
                                }.into_any()
                            }
                        }
                    })
                }}
            </Suspense>
        </section>
    }
}

fn campaign_card(c: &CampaignSummaryView, is_owner: bool) -> impl IntoView {
    let campaign_id = c.campaign_id.clone();
    let title = c.title.clone();
    let theme_id = c.theme_id.clone();
    let role_label = if is_owner { "Game Master" } else { "Player" };
    view! {
        <div class="campaign-library-card" data-testid="campaign-card">
            <a href=format!("/campaigns/{campaign_id}/lobby") class="campaign-card-link">
                <h3>{title}</h3>
                <p class="campaign-card-meta">
                    {role_label} " · " {theme_id}
                </p>
            </a>
        </div>
    }
}

fn invitation_card(i: &InvitationView) -> impl IntoView {
    let title = i.campaign_title.clone();
    let expires = i.expires_at.clone();
    view! {
        <div class="campaign-library-card" data-testid="invitation-card">
            <h3>{title}</h3>
            <p class="campaign-card-meta">"Invitation · expires " {expires}</p>
        </div>
    }
}

#[cfg(all(test, feature = "ssr"))]
mod tests {
    #[test]
    fn campaigns_module_compiles() {
        // The campaigns page uses Resource::new which requires a runtime executor.
        // We verify compilation here.
    }
}
