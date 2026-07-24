use leptos::prelude::*;
use leptos_meta::Title;
use leptos_router::hooks::use_params;
use leptos_router::params::Params;
use serde::{Deserialize, Serialize};

use crate::components::protected_layout::ProtectedLayout;
use crate::components::turn_history::TurnHistory;

#[derive(Params, PartialEq, Eq, Clone)]
struct PlayPageParams {
    id: String,
}

/// Lobby view data — populated from the campaign lobby server function.
/// This type mirrors the view API in `views/campaign_lobby/api.rs` and will be
/// replaced once that API is fully wired into campaign play.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(dead_code)]
pub struct PlayLobbyView {
    pub campaign_id: String,
    pub campaign_title: String,
    pub theme_name: String,
    pub is_gm: bool,
    pub is_active: bool,
    pub phase: Option<String>,
    pub active_member_name: Option<String>,
    pub active_character_name: Option<String>,
    pub active_account_id: Option<String>,
    pub round: Option<i32>,
    pub turn_number: Option<i32>,
    pub action_point_balance: Option<i32>,
    pub max_action_points: Option<i32>,
    pub members: Vec<PlayMemberView>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(dead_code)]
pub struct PlayMemberView {
    pub account_id: String,
    pub display_name: String,
    pub character_name: Option<String>,
    pub is_ready: bool,
    pub is_ai_substitute: bool,
}

/// Campaign Play page — the authenticated, turn-based play surface.
///
/// Route: `/campaigns/:id/play`
#[component]
pub fn CampaignPlayPage() -> impl IntoView {
    let params = use_params::<PlayPageParams>();
    let campaign_id = move || {
        params
            .read()
            .as_ref()
            .map(|p| p.id.clone())
            .unwrap_or_default()
    };

    let lobby_resource = Resource::new(
        campaign_id,
        move |id| async move { load_play_view(id).await },
    );

    view! {
        <ProtectedLayout>
            <Title text="Campaign Play"/>
            <Suspense fallback=|| {
                view! {
                    <div class="play-loading">
                        <p>"Loading play session…"</p>
                    </div>
                }
            }>
                {move || {
                    let lobby = match lobby_resource.get() {
                        Some(v) => v,
                        None => return view! { <p>"Loading…"</p> }.into_any(),
                    };
                    match lobby {
                        Ok(l) => {
                            if !l.is_active {
                                return view! {
                                    <div class="play-not-active">
                                        <h2>"Play session not active"</h2>
                                        <p>"The play session has not started yet."</p>
                                        <a href={format!("/campaigns/{}/lobby", l.campaign_id)}>
                                            "Go to lobby →"
                                        </a>
                                    </div>
                                }.into_any();
                            }
                            view! {
                                <CampaignPlayContent
                                    lobby={l}
                                />
                            }.into_any()
                        }
                        Err(e) => view! {
                            <div class="play-error">
                                <h2>"Unable to load play session"</h2>
                                <p>{e.to_string()}</p>
                                <a href="/campaigns">"Back to campaigns"</a>
                            </div>
                        }.into_any(),
                    }
                }}
            </Suspense>
        </ProtectedLayout>
    }
}

#[component]
fn CampaignPlayContent(lobby: PlayLobbyView) -> impl IntoView {
    let campaign_id = lobby.campaign_id.clone();
    let is_gm = lobby.is_gm;
    let current_phase = lobby.phase.clone().unwrap_or_else(|| "waiting".to_string());
    let active_actor = lobby
        .active_member_name
        .clone()
        .unwrap_or_else(|| "—".to_string());
    let active_character = lobby
        .active_character_name
        .clone()
        .unwrap_or_else(|| "—".to_string());

    view! {
        <div class="campaign-play">
            <header class="play-header">
                <div class="play-title">
                    <h1>{lobby.campaign_title.clone()}</h1>
                    <span class="play-theme">{lobby.theme_name.clone()}</span>
                </div>
                <div class="play-phase-indicator">
                    <span class="phase-label">"Current phase"</span>
                    <span class="phase-value">{current_phase.clone()}</span>
                </div>
            </header>

            <div class="play-layout">
                <aside class="play-sidebar">
                    <section class="play-status">
                        <h2>"Session Status"</h2>
                        <dl>
                            <div>
                                <dt>"Phase"</dt>
                                <dd>{current_phase.clone()}</dd>
                            </div>
                            <div>
                                <dt>"Active actor"</dt>
                                <dd>{active_actor.clone()}</dd>
                            </div>
                            <div>
                                <dt>"Active character"</dt>
                                <dd>{active_character.clone()}</dd>
                            </div>
                            <div>
                                <dt>"Round"</dt>
                                <dd>{lobby.round.unwrap_or(0)}</dd>
                            </div>
                            <div>
                                <dt>"Turn"</dt>
                                <dd>{lobby.turn_number.unwrap_or(0)}</dd>
                            </div>
                        </dl>
                    </section>

                    <section class="play-party">
                        <h2>"Party"</h2>
                        <ul class="party-list">
                            {lobby.members.iter().map(|m| {
                                let is_active = m.account_id
                                    == lobby.active_account_id.as_deref().unwrap_or("");
                                view! {
                                    <li class={if is_active { "party-member active" } else { "party-member" }}>
                                        <span class="member-name">{m.display_name.clone()}</span>
                                        {m.character_name.as_ref().map(|cn| {
                                            view! { <span class="member-character">{cn.clone()}</span> }
                                        })}
                                        {m.is_ai_substitute.then(|| {
                                            view! {
                                                <span class="ai-badge" title="AI-controlled this turn">
                                                    "AI"
                                                </span>
                                            }
                                        })}
                                        <span class={if m.is_ready { "ready-yes" } else { "ready-no" }}>
                                            {if m.is_ready { "✓ Ready" } else { "○ Not ready" }}
                                        </span>
                                    </li>
                                }
                            }).collect::<Vec<_>>()}
                        </ul>
                    </section>

                    <section class="play-action-points">
                        <h2>"Action Points"</h2>
                        <p class="balance">
                            <span class="balance-label">"Custom prompt balance"</span>
                            <span class="balance-value">
                                {lobby.action_point_balance.unwrap_or(0)}
                                "/"
                                {lobby.max_action_points.unwrap_or(3)}
                            </span>
                        </p>
                        <p class="cost-hint">
                            "Structured actions are free. Accepted custom prompts cost 1 point."
                        </p>
                    </section>
                </aside>

                <main class="play-main">
                    <PlayPhaseContent
                        campaign_id={campaign_id.clone()}
                        phase={current_phase.clone()}
                        is_gm
                    />

                    <TurnHistory campaign_id={campaign_id.clone()} />
                </main>
            </div>
        </div>
    }
}

#[component]
fn PlayPhaseContent(campaign_id: String, phase: String, is_gm: bool) -> impl IntoView {
    let _ = campaign_id;
    match phase.as_str() {
        "game_master_generation" => {
            let gm_controls = if is_gm {
                view! {
                    <div class="gm-controls">
                        <p>"You are the GM. Generate the scene when ready."</p>
                    </div>
                }
                .into_any()
            } else {
                view! {
                    <p class="waiting-status" role="status" aria-live="polite">
                        "Waiting for the Game Master."
                    </p>
                }
                .into_any()
            };
            view! {
                <section class="play-gm-phase">
                    <h2>"Game Master is narrating…"</h2>
                    <div class="gm-waiting">
                        <p>"The GM is preparing the next scene. Please wait."</p>
                        {gm_controls}
                    </div>
                </section>
            }
            .into_any()
        }

        "player_action" => view! {
            <section class="play-action-phase">
                <h2>"Your turn to act"</h2>
                <div class="play-action-prompt">
                    <p>"Select a legal action or enter a custom prompt."</p>
                    <p class="play-hint">"Action controls will appear here once the turn engine is wired."</p>
                </div>
            </section>
        }
        .into_any(),

        "resolving" => view! {
            <section class="play-resolving-phase">
                <h2>"Resolving…"</h2>
                <p class="waiting-status" role="status" aria-live="polite">
                    "The engine is resolving the current action."
                </p>
            </section>
        }
        .into_any(),

        _ => view! {
            <section class="play-unknown-phase">
                <h2>"Waiting…"</h2>
                <p>"The play session is in an unknown phase."</p>
            </section>
        }
        .into_any(),
    }
}

#[server(LoadPlayView)]
async fn load_play_view(campaign_id: String) -> Result<PlayLobbyView, ServerFnError> {
    // TODO: Wire to the lobby application service once Task 16/17 integration is complete.
    // For now, return a minimal placeholder so the page renders.
    Ok(PlayLobbyView {
        campaign_id,
        campaign_title: "Campaign".to_owned(),
        theme_name: String::new(),
        is_gm: false,
        is_active: false,
        phase: None,
        active_member_name: None,
        active_character_name: None,
        active_account_id: None,
        round: None,
        turn_number: None,
        action_point_balance: None,
        max_action_points: None,
        members: Vec::new(),
    })
}
