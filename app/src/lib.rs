mod campaign;

use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_meta::{MetaTags, Stylesheet, Title, provide_meta_context};
use leptos_router::{
    StaticSegment,
    components::{Route, Router, Routes},
};
use manchester_dnd_core::{
    AttemptExplorationCheckCommand, EXPLORATION_CHECK_SCHEMA_VERSION, ExplorationCheckOutcomeDto,
    LocalCampaignViewDto,
};

use crate::campaign::{
    CampaignLoadResponse, ExplorationCheckResponse, LOCAL_EXPLORATION_ACTION_ID, PublicGameError,
    attempt_exploration_check, load_local_campaign,
};

pub fn shell(options: LeptosOptions) -> impl IntoView {
    view! {
        <!DOCTYPE html>
        <html lang="en">
            <head>
                <meta charset="utf-8"/>
                <meta name="viewport" content="width=device-width, initial-scale=1"/>
                <meta
                    name="description"
                    content="A collaborative, AI-guided fantasy role-playing game inspired by Manchester."
                />
                <AutoReload options=options.clone()/>
                <HydrationScripts options/>
                <MetaTags/>
            </head>
            <body>
                <App/>
            </body>
        </html>
    }
}

#[component]
pub fn App() -> impl IntoView {
    provide_meta_context();

    view! {
        <Stylesheet id="leptos" href="/pkg/manchester-arcana.css"/>
        <Title text="Manchester Arcana"/>
        <Router>
            <Routes fallback=|| view! { <NotFound/> }.into_view()>
                <Route path=StaticSegment("") view=HomePage/>
            </Routes>
        </Router>
    }
}

#[component]
fn HomePage() -> impl IntoView {
    let selected_theme = RwSignal::new("Canal Warden");
    let campaign_view = RwSignal::new(None::<LocalCampaignViewDto>);
    let campaign_loading = RwSignal::new(true);
    let roll_pending = RwSignal::new(false);
    let retry_command = RwSignal::new(None::<AttemptExplorationCheckCommand>);
    let roll_summary = RwSignal::new(String::from("Loading the saved local campaign…"));

    Effect::new(move |_| {
        load_campaign_into(campaign_view, campaign_loading, roll_summary);
    });

    let roll_check = move |_| {
        let Some(view) = campaign_view.get_untracked() else {
            roll_summary.set("The campaign is not ready yet.".to_owned());
            return;
        };
        let command =
            retry_command
                .get_untracked()
                .unwrap_or_else(|| AttemptExplorationCheckCommand {
                    schema_version: EXPLORATION_CHECK_SCHEMA_VERSION,
                    campaign_session_id: view.campaign_session_id,
                    character_id: view.character_id,
                    action_id: LOCAL_EXPLORATION_ACTION_ID.to_owned(),
                    expected_revision: view.revision,
                    idempotency_key: uuid::Uuid::new_v4().to_string(),
                });
        retry_command.set(Some(command.clone()));
        roll_pending.set(true);
        roll_summary.set("The server is resolving and saving the check…".to_owned());

        spawn_local(async move {
            match attempt_exploration_check(command).await {
                Ok(ExplorationCheckResponse::Committed(outcome)) => {
                    roll_summary.set(format_check(&outcome));
                    campaign_view.update(|current| {
                        if let Some(view) = current {
                            view.revision = outcome.result_revision;
                            view.last_event_sequence = outcome.event_sequence;
                            view.latest_check = Some(outcome);
                        }
                    });
                    retry_command.set(None);
                }
                Ok(ExplorationCheckResponse::Rejected(error)) => {
                    let stale = error.code == "revision_conflict";
                    roll_summary.set(format_public_error(&error));
                    if stale || !error.retryable {
                        retry_command.set(None);
                    }
                    if stale {
                        load_campaign_into(campaign_view, campaign_loading, roll_summary);
                    }
                }
                Err(_) => {
                    roll_summary.set(
                        "The request was interrupted. Retry will reuse the same command so it cannot commit twice."
                            .to_owned(),
                    );
                }
            }
            roll_pending.set(false);
        });
    };

    let refresh_campaign = move |_| {
        retry_command.set(None);
        load_campaign_into(campaign_view, campaign_loading, roll_summary);
    };

    view! {
        <main class="game-shell">
            <header class="topbar">
                <a class="brand" href="/" aria-label="Manchester Arcana home">
                    <span class="brand-mark">"M"</span>
                    <span>
                        <strong>"Manchester Arcana"</strong>
                        <small>"An AI-guided 5E-compatible adventure"</small>
                    </span>
                </a>
                <div class="status-pill"><span></span>"Local campaign"</div>
            </header>

            <section class="hero-grid">
                <div class="hero-copy">
                    <p class="eyebrow">"THE RAIN REMEMBERS"</p>
                    <h1>"Your city. Your stories. A realm remade."</h1>
                    <p class="lede">
                        "Build a hero, gather your party, and let an AI game master weave original fantasy with private, consented fragments of real life."
                    </p>
                    <div class="hero-actions">
                        <a class="primary-button" href="#themes">"Create your hero"</a>
                        <a class="text-link" href="#themes">"Explore character themes →"</a>
                    </div>
                </div>

                <aside class="scene-card">
                    <div class="scene-glow"></div>
                    <p class="scene-label">"Tonight's omen"</p>
                    <blockquote>
                        "Beneath the old viaduct, a brass tram bell sounds once—though the tracks have been cold for a century."
                    </blockquote>
                    <div class="scene-meta">
                        <span>"Narrative preview"</span>
                        <span>"Awaiting player action"</span>
                    </div>
                </aside>
            </section>

            <section class="command-grid" id="themes">
                <article class="panel theme-panel">
                    <div class="panel-heading">
                        <div>
                            <p class="eyebrow">"CHARACTER FORGE"</p>
                            <h2>"Choose a story lens"</h2>
                        </div>
                        <span class="step">"01 / 04"</span>
                    </div>

                    <div class="theme-list">
                        <ThemeButton
                            name="Canal Warden"
                            detail="Steadfast guardian · exploration"
                            selected_theme
                        />
                        <ThemeButton
                            name="Rainbound Occultist"
                            detail="Curious scholar · strange magic"
                            selected_theme
                        />
                        <ThemeButton
                            name="Clockwork Troubadour"
                            detail="Quick-witted envoy · social play"
                            selected_theme
                        />
                    </div>

                    <p class="selection-copy">
                        "Selected: " <strong>{move || selected_theme.get()}</strong>
                    </p>
                </article>

                <article class="panel rules-panel">
                    <div class="panel-heading">
                        <div>
                            <p class="eyebrow">"RULES ENGINE"</p>
                            <h2>"Trust the roll"</h2>
                        </div>
                        <span class="die-icon">"d20"</span>
                    </div>
                    <p>
                        "Checks, attacks, action resources, experience thresholds, and level derivation are resolved by deterministic Rust rules—not improvised by the model."
                    </p>
                    <ul class="rule-list">
                        <li><span>"01"</span>"The server makes authoritative rolls."</li>
                        <li><span>"02"</span>"Saved turn events are schema-versioned."</li>
                        <li><span>"03"</span>"Campaigns retain their ruleset version."</li>
                    </ul>
                    <div class="roll-demo">
                        <p class="roll-label">"Inspect the viaduct runes · Wisdom (Perception)"</p>
                        <p class="save-status">
                            {move || campaign_view.get().map_or_else(
                                || "Campaign unavailable".to_owned(),
                                |view| format!(
                                    "{} · {} · saved revision {}",
                                    view.campaign_title, view.character_name, view.revision
                                ),
                            )}
                        </p>
                        <button
                            class="roll-button"
                            disabled=move || roll_pending.get()
                                || campaign_loading.get()
                                || campaign_view.get().is_none()
                            on:click=roll_check
                        >
                            {move || if roll_pending.get() {
                                "Resolving and saving…"
                            } else if retry_command.get().is_some() {
                                "Retry the same action"
                            } else {
                                "Inspect the runes"
                            }}
                        </button>
                        <button
                            class="refresh-button"
                            disabled=move || campaign_loading.get() || roll_pending.get()
                            on:click=refresh_campaign
                        >
                            {move || if campaign_loading.get() {
                                "Loading save…"
                            } else {
                                "Reload saved turn"
                            }}
                        </button>
                        <p
                            class="roll-readout"
                            aria-live="polite"
                            aria-busy=move || roll_pending.get() || campaign_loading.get()
                        >
                            {move || roll_summary.get()}
                        </p>
                    </div>
                </article>

                <article class="panel privacy-panel">
                    <p class="eyebrow">"MEMORY, WITH BOUNDARIES"</p>
                    <h2>"Real stories stay under your control."</h2>
                    <p>
                        "Private Markdown event packs default to disabled, require explicit consent and sensitivity allowlists, and are excluded from version control by default. Campaign controls for editing and deletion are planned next."
                    </p>
                    <div class="privacy-tags">
                        <span>"Consent gates"</span><span>"Cooldowns"</span><span>"Private by default"</span>
                    </div>
                </article>
            </section>
        </main>
    }
}

fn load_campaign_into(
    campaign_view: RwSignal<Option<LocalCampaignViewDto>>,
    campaign_loading: RwSignal<bool>,
    roll_summary: RwSignal<String>,
) {
    campaign_loading.set(true);
    spawn_local(async move {
        match load_local_campaign().await {
            Ok(CampaignLoadResponse::Ready(view)) => {
                let summary = view.latest_check.as_ref().map_or_else(
                    || {
                        "The viaduct is waiting. This first action will be rolled and saved by the server."
                            .to_owned()
                    },
                    format_check,
                );
                campaign_view.set(Some(view));
                roll_summary.set(summary);
            }
            Ok(CampaignLoadResponse::Rejected(error)) => {
                campaign_view.set(None);
                roll_summary.set(format_public_error(&error));
            }
            Err(_) => {
                campaign_view.set(None);
                roll_summary.set(
                    "The saved campaign could not be reached. Check the local server and try again."
                        .to_owned(),
                );
            }
        }
        campaign_loading.set(false);
    });
}

fn format_check(outcome: &ExplorationCheckOutcomeDto) -> String {
    let result = &outcome.result;
    let dice = result.roll.second.map_or_else(
        || result.roll.first.to_string(),
        |second| {
            format!(
                "{} and {} → {}",
                result.roll.first, second, result.roll.selected
            )
        },
    );
    let result_label = if result.success { "success" } else { "setback" };
    format!(
        "Saved roll {dice}; {} {:+} ability + {} proficiency {:+} situational = {} vs DC {} — {result_label}. Revision {}.",
        result.roll.selected,
        result.ability_modifier,
        result.proficiency_modifier,
        result.situational_modifier,
        result.total,
        result.difficulty_class,
        outcome.result_revision,
    )
}

fn format_public_error(error: &PublicGameError) -> String {
    format!(
        "{} [{}; reference {}]",
        error.message, error.code, error.correlation_id
    )
}

#[component]
fn ThemeButton(
    name: &'static str,
    detail: &'static str,
    selected_theme: RwSignal<&'static str>,
) -> impl IntoView {
    view! {
        <button
            class="theme-button"
            class:selected=move || selected_theme.get() == name
            on:click=move |_| selected_theme.set(name)
        >
            <span class="theme-sigil">{name.chars().next().unwrap_or('M')}</span>
            <span><strong>{name}</strong><small>{detail}</small></span>
            <span class="theme-arrow">"↗"</span>
        </button>
    }
}

#[component]
fn NotFound() -> impl IntoView {
    view! {
        <main class="not-found">
            <p class="eyebrow">"LOST IN THE MISTS"</p>
            <h1>"That path is not on the map."</h1>
            <a class="primary-button" href="/">"Return to Manchester"</a>
        </main>
    }
}
