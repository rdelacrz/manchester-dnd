mod api;

use leptos::prelude::*;
use leptos_meta::Title;
use leptos_router::hooks::use_params;
use leptos_router::params::Params;

use self::api::{LobbyCharacterOption, LobbyMemberView, LobbyResponse, load_lobby};
use crate::components::protected_layout::ProtectedLayout;

#[derive(Params, PartialEq, Eq, Clone)]
struct LobbyPageParams {
    id: String,
}

/// Campaign Lobby page — lets members select characters, ready up,
/// and start play according to policy.
///
/// Route: `/campaigns/:id/lobby`
#[component]
pub fn CampaignLobbyPage() -> impl IntoView {
    let params = use_params::<LobbyPageParams>();
    let campaign_id = move || {
        params
            .read()
            .as_ref()
            .map(|p| p.id.clone())
            .unwrap_or_default()
    };

    let lobby = Resource::new(campaign_id, move |id| async move { load_lobby(id).await });

    view! {
        <ProtectedLayout>
            <Title text="Campaign Lobby"/>
            <Suspense fallback=|| {
                view! { <div class="lobby-loading"><p>"Loading lobby…"</p></div> }
            }>
                {move || {
                    let l = match lobby.get() {
                        Some(v) => v,
                        None => return view! { <p>"Loading…"</p> }.into_any(),
                    };
                    match l {
                        Ok(LobbyResponse::Ready {
                            campaign_id,
                            campaign_title,
                            theme_id,
                            is_gm,
                            play_session_state,
                            start_policy,
                            members,
                            available_characters,
                            assigned_character_id,
                            is_ready,
                            not_ready_members,
                        }) => {
                            view! {
                                <LobbyContent
                                    campaign_id={campaign_id}
                                    campaign_title={campaign_title}
                                    theme_id={theme_id}
                                    is_gm={is_gm}
                                    play_session_state={play_session_state}
                                    start_policy={start_policy}
                                    members={members}
                                    available_characters={available_characters}
                                    assigned_character_id={assigned_character_id}
                                    is_ready={is_ready}
                                    not_ready_members={not_ready_members}
                                />
                            }
                                .into_any()
                        }
                        Ok(LobbyResponse::NotFound) => view! {
                            <div class="lobby-not-found">
                                <h2>"Campaign not found"</h2>
                                <a href="/campaigns">"Back to campaigns"</a>
                            </div>
                        }
                            .into_any(),
                        Ok(LobbyResponse::AuthenticationRequired) => view! {
                            <div class="lobby-auth-required">
                                <h2>"Authentication required"</h2>
                                <a href="/login">"Sign in"</a>
                            </div>
                        }
                            .into_any(),
                        Ok(LobbyResponse::Error { message, .. }) => view! {
                            <div class="lobby-error">
                                <h2>"Unable to load lobby"</h2>
                                <p>{message}</p>
                                <a href="/campaigns">"Back to campaigns"</a>
                            </div>
                        }
                            .into_any(),
                        Err(e) => view! {
                            <div class="lobby-error">
                                <h2>"Unable to load lobby"</h2>
                                <p>{e.to_string()}</p>
                                <a href="/campaigns">"Back to campaigns"</a>
                            </div>
                        }
                            .into_any(),
                    }
                }}
            </Suspense>
        </ProtectedLayout>
    }
}

#[component]
#[allow(clippy::too_many_arguments)]
fn LobbyContent(
    campaign_id: String,
    campaign_title: String,
    theme_id: String,
    is_gm: bool,
    play_session_state: String,
    start_policy: Option<String>,
    members: Vec<LobbyMemberView>,
    available_characters: Vec<LobbyCharacterOption>,
    assigned_character_id: Option<String>,
    is_ready: bool,
    not_ready_members: Vec<String>,
) -> impl IntoView {
    let is_active = play_session_state == "active";

    view! {
        <div class="campaign-lobby">
            <header class="lobby-header">
                <h1>{campaign_title.clone()}</h1>
                <span class="lobby-theme">{theme_id.clone()}</span>
            </header>

            {if is_active {
                view! {
                    <div class="lobby-active-banner">
                        <p>"Play session is active."</p>
                        <a href={format!("/campaigns/{}/play", campaign_id)}>
                            "Enter play →"
                        </a>
                    </div>
                }.into_any()
            } else {
                view! {
                    <div class="lobby-members">
                        <h2>"Members"</h2>
                        <ul class="member-list">
                            {members.iter().map(|m| {
                                view! {
                                    <li class="member-row">
                                        <span class="member-name">{m.display_name.clone()}</span>
                                        {m.character_name.as_ref().map(|c| {
                                            view! { <span class="member-character">{c.clone()}</span> }
                                        })}
                                        <span class={if m.is_ready { "ready-yes" } else { "ready-no" }}>
                                            {if m.is_ready { "✓ Ready" } else { "○ Not ready" }}
                                        </span>
                                        {(m.role == "game_master").then(|| {
                                            view! { <span class="gm-badge">"GM"</span> }
                                        })}
                                    </li>
                                }
                            }).collect::<Vec<_>>()}
                        </ul>
                    </div>
                }.into_any()
            }}

            {if is_gm && !is_active {
                view! {
                    <div class="gm-controls">
                        <h2>"GM Controls"</h2>
                        <p>"Start the play session when all members are ready."</p>
                        {if !not_ready_members.is_empty() {
                            view! {
                                <p class="not-ready-warning">
                                    "Waiting for "{not_ready_members.len()}" member(s) to ready up."
                                </p>
                            }.into_any()
                        } else {
                            view! {
                                <p class="all-ready">"All members are ready."</p>
                            }.into_any()
                        }}
                    </div>
                }.into_any()
            } else {
                view! { <div></div> }.into_any()
            }}

            {if !is_active && !available_characters.is_empty() {
                view! {
                    <div class="character-selection">
                        <h2>"Your Character"</h2>
                        <p>"Assigned character: "{assigned_character_id.clone().unwrap_or_else(|| "None".to_string())}</p>
                        <p>"Ready: "{if is_ready { "Yes" } else { "No" }}</p>
                        {if start_policy.is_some() {
                            view! {
                                <p class="start-policy">
                                    "Start policy: "{start_policy.unwrap()}
                                </p>
                            }.into_any()
                        } else {
                            view! { <div></div> }.into_any()
                        }}
                    </div>
                }.into_any()
            } else {
                view! { <div></div> }.into_any()
            }}
        </div>
    }
}
