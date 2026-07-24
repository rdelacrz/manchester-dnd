use leptos::prelude::*;
use leptos::task::spawn_local;
use manchester_dnd_core::{
    ADVANCE_NPC_TURN_SCHEMA_VERSION, AdvanceNpcTurnCommand, AttemptExplorationCheckCommand,
    AttemptSocialInteractionCommand, CommitEncounterCommand, CommittedEncounterOutcomeDto,
    ENCOUNTER_COMMIT_SCHEMA_VERSION, EXPLORATION_CHECK_SCHEMA_VERSION, ExplorationCheckOutcomeDto,
    LocalCampaignViewDto, SOCIAL_INTERACTION_SCHEMA_VERSION, SocialInteractionOutcomeDto,
    encounter::{
        EncounterCommand, EncounterIntent, EncounterState, EncounterStatus, LegalEncounterAction,
        ObjectiveStatus,
    },
    hero::SpellId,
    rules_matrix::{D20TestOutcome, NpcAttitude, ProgressStatus},
};

use crate::app::FirstRunStep;
use crate::components::campaign::{
    CampaignLoadResponse, EncounterCommandResponse, ExplorationCheckResponse,
    LOCAL_EXPLORATION_ACTION_ID, LOCAL_SOCIAL_ACTION_ID, PublicGameError,
    SocialInteractionResponse, advance_npc_turn, attempt_exploration_check,
    attempt_social_interaction, load_local_campaign, submit_encounter_action,
};
use crate::components::freeform::{FreeformIntent, FreeformIntentState};
use crate::components::hero::HeroCreator;
use crate::components::images::SceneImagePanel;
use crate::components::lifecycle::CampaignLifecyclePanel;
use crate::components::privacy::PrivacyControls;

#[component]
pub fn Home() -> impl IntoView {
    let campaign_view = RwSignal::new(None::<LocalCampaignViewDto>);
    let campaign_loading = RwSignal::new(true);
    let roll_pending = RwSignal::new(false);
    let retry_command = RwSignal::new(None::<AttemptExplorationCheckCommand>);
    let roll_summary = RwSignal::new(String::from("Loading the saved local campaign…"));
    let social_pending = RwSignal::new(false);
    let social_retry = RwSignal::new(None::<AttemptSocialInteractionCommand>);
    let social_notice = RwSignal::new(String::from(
        "Speak with the lockkeeper before inspecting the runes, or continue directly to exploration.",
    ));
    // Lifecycle refreshes must not overwrite the persisted roll/result
    // presentation. This signal is intentionally separate from roll_summary.
    let lifecycle_campaign_notice = RwSignal::new(String::new());
    let encounter_pending = RwSignal::new(false);
    let encounter_retry = RwSignal::new(None::<CommitEncounterCommand>);
    let npc_retry = RwSignal::new(None::<AdvanceNpcTurnCommand>);
    let encounter_notice = RwSignal::new(String::from(
        "Inspect the runes to reveal what waits beneath the viaduct.",
    ));
    let reduced_motion = RwSignal::new(false);
    let compact_density = RwSignal::new(false);
    let animate_dice = RwSignal::new(true);
    let freeform_intent_state = FreeformIntentState::new();

    Effect::new(move |_| {
        load_campaign_into(campaign_view, campaign_loading, roll_summary);
    });
    Effect::new(move |_| {
        if let Some(value) = stored_preference("reduced-motion") {
            reduced_motion.set(value);
        }
        if let Some(value) = stored_preference("compact-density") {
            compact_density.set(value);
        }
        if let Some(value) = stored_preference("animate-dice") {
            animate_dice.set(value);
        }
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
                    retry_command.set(None);
                    encounter_retry.set(None);
                    npc_retry.set(None);
                    encounter_notice.set(
                        "The consequence is saved. The Soot Wight encounter is ready.".to_owned(),
                    );
                    load_campaign_into(campaign_view, campaign_loading, roll_summary);
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

    let resolve_social = move |_| {
        let Some(view) = campaign_view.get_untracked() else {
            social_notice.set("The campaign is not ready yet.".to_owned());
            return;
        };
        let command =
            social_retry
                .get_untracked()
                .unwrap_or_else(|| AttemptSocialInteractionCommand {
                    schema_version: SOCIAL_INTERACTION_SCHEMA_VERSION,
                    campaign_session_id: view.campaign_session_id,
                    character_id: view.character_id,
                    action_id: LOCAL_SOCIAL_ACTION_ID.to_owned(),
                    expected_revision: view.revision,
                    idempotency_key: uuid::Uuid::new_v4().to_string(),
                });
        social_retry.set(Some(command.clone()));
        social_pending.set(true);
        social_notice.set("The server is resolving and saving the social check…".to_owned());
        spawn_local(async move {
            match attempt_social_interaction(command).await {
                Ok(SocialInteractionResponse::Committed(outcome)) => {
                    social_notice.set(format_social_outcome(&outcome));
                    social_retry.set(None);
                    load_campaign_into(campaign_view, campaign_loading, roll_summary);
                }
                Ok(SocialInteractionResponse::Rejected(error)) => {
                    let stale = error.code == "revision_conflict";
                    social_notice.set(format_public_error(&error));
                    if stale || !error.retryable {
                        social_retry.set(None);
                    }
                    if stale {
                        load_campaign_into(campaign_view, campaign_loading, roll_summary);
                    }
                }
                Err(_) => social_notice.set(
                    "The request was interrupted. Retry reuses the same social command.".to_owned(),
                ),
            }
            social_pending.set(false);
        });
    };

    let refresh_campaign = move |_| {
        retry_command.set(None);
        social_retry.set(None);
        encounter_retry.set(None);
        npc_retry.set(None);
        encounter_notice.set("Reloading the authoritative encounter…".to_owned());
        load_campaign_into(campaign_view, campaign_loading, roll_summary);
    };

    view! {
        <main
            id="main-content"
            class="game-shell"
            class:reduced-motion=move || reduced_motion.get()
            class:compact-density=move || compact_density.get()
            class:static-dice=move || !animate_dice.get()
            tabindex="-1"
        >
            <header class="topbar">
                <a class="brand" href="/" aria-label="Manchester Arcana home">
                    <span class="brand-mark">"M"</span>
                    <span>
                        <strong>"Manchester Arcana"</strong>
                        <small>"An AI-guided 5E-compatible adventure"</small>
                    </span>
                </a>
                <nav aria-label="Primary navigation">
                    <a href="#play">"Play"</a>
                    <a href="#themes">"Hero"</a>
                    <a href="#privacy">"Safety"</a>
                </nav>
                <div class="status-pill" role="status"><span></span>"Local campaign"</div>
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

            <section class="panel first-run-panel" id="getting-started" aria-labelledby="getting-started-heading">
                <div>
                    <p class="eyebrow">"FIRST RUN"</p>
                    <h2 id="getting-started-heading">"Six saved steps to your first adventure"</h2>
                    <p>"Create or resume the campaign, forge a hero, speak with the lockkeeper, inspect the runes, finish the encounter, then review and export the saved history."</p>
                </div>
                <ol>
                    <FirstRunStep><a href="#campaigns">"Campaign"</a></FirstRunStep>
                    <FirstRunStep><a href="#themes">"Hero"</a></FirstRunStep>
                    <FirstRunStep><a href="#social">"Social scene"</a></FirstRunStep>
                    <FirstRunStep><a href="#play">"Exploration"</a></FirstRunStep>
                    <FirstRunStep><a href="#encounter">"Encounter"</a></FirstRunStep>
                    <FirstRunStep><a href="#campaigns">"History and export"</a></FirstRunStep>
                </ol>
                <p><a class="text-link" href="/guide">"Read safe setup, supported features, and known limits →"</a></p>
            </section>

            <CampaignLifecyclePanel
                campaign_view
                campaign_loading
                campaign_notice=lifecycle_campaign_notice
            />

            <section class="command-grid" id="themes" aria-labelledby="forge-heading">
                <HeroCreator
                    campaign_view
                    campaign_loading
                    campaign_notice=roll_summary
                />

                <article class="panel rules-panel social-panel" id="social" aria-labelledby="social-heading">
                    <div class="panel-heading">
                        <div>
                            <p class="eyebrow">"AUTHORED SOCIAL SCENE"</p>
                            <h2 id="social-heading">"The lockkeeper at the rain gate"</h2>
                        </div>
                        <span class="die-icon">"d20"</span>
                    </div>
                    <p>
                        "Ask Lockkeeper Elin for safe passage. The server maps a fixed Moderate tier to DC 15, rolls Charisma (Persuasion), and saves the objective, threat clock, and NPC attitude."
                    </p>
                    {move || {
                        let Some(view) = campaign_view.get() else {
                            return view! { <p class="save-status">"Campaign unavailable"</p> }
                                .into_any();
                        };
                        let Some(social) = view.social else {
                            return view! {
                                <p class="save-status">"Finish hero creation to unlock the social scene."</p>
                            }
                            .into_any();
                        };
                        let objective = social.state.objectives.first().map_or(
                            "Unavailable".to_owned(),
                            |objective| match objective.status {
                                ProgressStatus::Active => format!(
                                    "Trust objective: {}/{}",
                                    objective.progress, objective.target
                                ),
                                ProgressStatus::Completed => "Trust objective: completed".to_owned(),
                                ProgressStatus::Failed => "Trust objective: failed".to_owned(),
                            },
                        );
                        let clock = social.state.clocks.first().map_or(
                            "Threat clock unavailable".to_owned(),
                            |clock| format!("Soot tide: {}/{}", clock.filled, clock.segments),
                        );
                        let attitude = social.state.npcs.first().map_or(
                            "Unknown".to_owned(),
                            |npc| match npc.attitude {
                                NpcAttitude::Hostile => "Hostile".to_owned(),
                                NpcAttitude::Indifferent => "Indifferent".to_owned(),
                                NpcAttitude::Friendly => "Friendly".to_owned(),
                            },
                        );
                        let resolved = social.latest_outcome.is_some();
                        let exploration_started = view.latest_check.is_some();
                        view! {
                            <dl class="encounter-meta social-state">
                                <div><dt>"Objective"</dt><dd>{objective}</dd></div>
                                <div><dt>"Threat clock"</dt><dd>{clock}</dd></div>
                                <div><dt>"Elin's attitude"</dt><dd>{attitude}</dd></div>
                                <div><dt>"Social turn"</dt><dd>{social.state.turn}</dd></div>
                            </dl>
                            <button
                                class="encounter-action social-action"
                                disabled=social_pending.get() || resolved || exploration_started
                                on:click=resolve_social
                            >
                                {if resolved {
                                    "Conversation already saved"
                                } else if exploration_started {
                                    "Exploration already underway"
                                } else if social_retry.get().is_some() {
                                    "Retry exact conversation"
                                } else {
                                    "Ask Elin about the sealed rain gate"
                                }}
                            </button>
                            <p
                                class="social-notice"
                                role="status"
                                aria-live="polite"
                                aria-busy=social_pending.get()
                            >
                                {social.latest_outcome.as_ref().map_or_else(
                                    || social_notice.get(),
                                    format_social_outcome,
                                )}
                            </p>
                        }
                        .into_any()
                    }}
                </article>

                <article class="panel rules-panel" id="play">
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
                                    "{} · {} · saved revision {}{}",
                                    view.campaign_title,
                                    view.character_name,
                                    view.revision,
                                    if view.content_pins.sealed().is_some() {
                                        ""
                                    } else {
                                        " · finish hero theme setup before play"
                                    }
                                ),
                            )}
                        </p>
                        <button
                            class="roll-button"
                            disabled=move || roll_pending.get()
                                || campaign_loading.get()
                                || campaign_view.get().is_none()
                                || campaign_view.get().is_some_and(|view| view.content_pins.sealed().is_none())
                                || campaign_view.get().is_some_and(|view| view.encounter.is_some())
                            on:click=roll_check
                        >
                            {move || if roll_pending.get() {
                                "Resolving and saving…"
                            } else if campaign_view.get().is_some_and(|view| view.content_pins.sealed().is_none()) {
                                "Create your hero before play"
                            } else if campaign_view.get().is_some_and(|view| view.encounter.is_some()) {
                                "Runes already resolved"
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

                <EncounterPanel
                    campaign_view
                    campaign_loading
                    roll_summary
                    encounter_pending
                    encounter_retry
                    npc_retry
                    encounter_notice
                    freeform_intent_state
                />

                <PrivacyControls campaign_view freeform_intent_state/>

                <SceneImagePanel campaign_view/>

                <aside class="panel preferences-panel" aria-labelledby="preferences-heading">
                    <div>
                        <p class="eyebrow">"YOUR DISPLAY"</p>
                        <h2 id="preferences-heading">"Presentation preferences"</h2>
                        <p>"These settings stay in this browser and never change saved game mechanics."</p>
                    </div>
                    <fieldset>
                        <legend>"Display options"</legend>
                        <label>
                            <input
                                type="checkbox"
                                checked=move || reduced_motion.get()
                                on:change=move |event| {
                                    let value = event_target_checked(&event);
                                    reduced_motion.set(value);
                                    store_preference("reduced-motion", value);
                                }
                            />
                            "Reduce motion"
                        </label>
                        <label>
                            <input
                                type="checkbox"
                                checked=move || compact_density.get()
                                on:change=move |event| {
                                    let value = event_target_checked(&event);
                                    compact_density.set(value);
                                    store_preference("compact-density", value);
                                }
                            />
                            "Compact layout"
                        </label>
                        <label>
                            <input
                                type="checkbox"
                                checked=move || animate_dice.get()
                                on:change=move |event| {
                                    let value = event_target_checked(&event);
                                    animate_dice.set(value);
                                    store_preference("animate-dice", value);
                                }
                            />
                            "Animate dice"
                        </label>
                    </fieldset>
                </aside>
            </section>
            <footer>
                <p>"Private evaluation build · Manchester Arcana is a working title."</p>
                <div class="footer-links">
                    <a href="/guide">"Supported features"</a>
                    <a href="/privacy-and-safety">"Privacy and reporting"</a>
                    <a href="/legal">"Legal and attribution"</a>
                </div>
            </footer>
        </main>
    }
}

#[derive(Clone, Copy)]
struct EncounterUiSignals {
    campaign_view: RwSignal<Option<LocalCampaignViewDto>>,
    campaign_loading: RwSignal<bool>,
    roll_summary: RwSignal<String>,
    encounter_pending: RwSignal<bool>,
    encounter_retry: RwSignal<Option<CommitEncounterCommand>>,
    npc_retry: RwSignal<Option<AdvanceNpcTurnCommand>>,
    encounter_notice: RwSignal<String>,
}

#[component]
fn EncounterPanel(
    campaign_view: RwSignal<Option<LocalCampaignViewDto>>,
    campaign_loading: RwSignal<bool>,
    roll_summary: RwSignal<String>,
    encounter_pending: RwSignal<bool>,
    encounter_retry: RwSignal<Option<CommitEncounterCommand>>,
    npc_retry: RwSignal<Option<AdvanceNpcTurnCommand>>,
    encounter_notice: RwSignal<String>,
    freeform_intent_state: FreeformIntentState,
) -> impl IntoView {
    let ui = EncounterUiSignals {
        campaign_view,
        campaign_loading,
        roll_summary,
        encounter_pending,
        encounter_retry,
        npc_retry,
        encounter_notice,
    };
    view! {
        <article class="panel encounter-panel" id="encounter" aria-labelledby="encounter-heading">
            <div class="panel-heading">
                <div>
                    <p class="eyebrow">"SAVED ENCOUNTER"</p>
                    <h2 id="encounter-heading">"The Soot Beneath the Viaduct"</h2>
                </div>
                <span class="die-icon">"d20"</span>
            </div>

            {move || {
                let Some(view) = campaign_view.get() else {
                    return view! {
                        <div class="encounter-empty" role="status">
                            <p>"Loading the authoritative campaign…"</p>
                        </div>
                    }
                    .into_any();
                };
                let Some(encounter) = view.encounter else {
                    return view! {
                        <div class="encounter-empty">
                            <p>
                                "Inspect the viaduct runes first. That saved result determines the encounter's opening consequence."
                            </p>
                            <a class="text-link" href="#play">"Return to the rune check ↑"</a>
                        </div>
                    }
                    .into_any();
                };

                let state = encounter.state.clone();
                let actor_name = state
                    .current_actor()
                    .map(|actor| actor.name.clone())
                    .unwrap_or_else(|| "Awaiting initiative".to_owned());
                let initiative = state.initiative.as_ref().map_or_else(
                    || "Not rolled".to_owned(),
                    |initiative| {
                        initiative
                            .order
                            .iter()
                            .filter_map(|id| {
                                if id == &state.hero.id {
                                    Some(state.hero.name.as_str())
                                } else if id == &state.creature.id {
                                    Some(state.creature.name.as_str())
                                } else {
                                    None
                                }
                            })
                            .collect::<Vec<_>>()
                            .join(" → ")
                    },
                );
                let turn_resources = state.turn_resources.as_ref().map_or_else(
                    || "Resources begin after initiative.".to_owned(),
                    |resources| {
                        format!(
                            "Movement {} ft · action {} · bonus action {} · reaction {} · object interaction {}",
                            resources.movement_remaining_feet,
                            availability(resources.action_available),
                            availability(resources.bonus_action_available),
                            availability(resources.reaction_available),
                            availability(resources.object_interaction_available),
                        )
                    },
                );
                let objective = match state.objectives.primary.status {
                    ObjectiveStatus::Pending => "Defeat the Soot Wight",
                    ObjectiveStatus::Completed => "Soot Wight defeated",
                    ObjectiveStatus::Failed => "The objective was not completed",
                };
                let encounter_status = encounter_status_label(state.status);
                let narration = encounter.latest_outcome.as_ref().map_or_else(
                    || {
                        "The rune consequence is fixed. Roll initiative when you are ready."
                            .to_owned()
                    },
                    |outcome| outcome.resolution.narration.authored_text.clone(),
                );
                let roll_explanations = encounter
                    .latest_outcome
                    .as_ref()
                    .map(format_roll_explanations)
                    .unwrap_or_default();
                let has_roll_explanations = !roll_explanations.is_empty();
                let npc_turn = state.status == EncounterStatus::Active
                    && state.current_actor_id.as_deref() == Some(state.creature.id.as_str())
                    && !state.live_q04.as_ref().is_some_and(|live| {
                        live.pending_attack_reaction.is_some()
                    });
                let actions = encounter
                    .legal_actions
                    .iter()
                    .filter_map(|action| encounter_action_spec(action, &state))
                    .map(|(label, intent)| {
                        let intent_for_click = intent.clone();
                        view! {
                            <button
                                class="encounter-action"
                                disabled=move || encounter_pending.get()
                                on:click=move |_| {
                                    submit_encounter_intent(ui, Some(intent_for_click.clone()));
                                }
                            >
                                {label}
                            </button>
                        }
                    })
                    .collect_view();

                view! {
                    <div class="encounter-scene" aria-label="Encounter state">
                        <p class="encounter-narration">{narration}</p>
                        <dl class="encounter-meta">
                            <div><dt>"Status"</dt><dd>{encounter_status}</dd></div>
                            <div><dt>"Round"</dt><dd>{state.round}</dd></div>
                            <div><dt>"Current actor"</dt><dd>{actor_name}</dd></div>
                            <div><dt>"Initiative"</dt><dd>{initiative}</dd></div>
                            <div><dt>"Objective"</dt><dd>{objective}</dd></div>
                            <div><dt>"Encounter revision"</dt><dd>{state.revision}</dd></div>
                        </dl>

                        <div class="combatants">
                            <CombatantCard combatant=state.hero.clone()/>
                            <CombatantCard combatant=state.creature.clone()/>
                        </div>

                        <p class="turn-resources">{turn_resources}</p>
                        <p class="turn-resources live-rules-resources">
                            {live_rules_resource_summary(&state)}
                        </p>
                        <div class="encounter-actions" aria-label="Legal encounter actions">
                            {actions}
                        </div>
                        <Show when=move || npc_turn>
                            <div class="npc-turn-control" role="group" aria-labelledby="npc-turn-heading">
                                <h3 id="npc-turn-heading">"Deterministic creature turn"</h3>
                                <p>
                                    "The server chooses the Soot Wight's next legal step. No creature action, target, destination, or roll is selected by this browser."
                                </p>
                                <button
                                    class="encounter-action npc-advance-action"
                                    disabled=move || encounter_pending.get()
                                    on:click=move |_| {
                                        submit_npc_advance(ui, false);
                                    }
                                >
                                    "Advance Soot Wight by server policy"
                                </button>
                            </div>
                        </Show>
                        <FreeformIntent
                            state=freeform_intent_state
                            campaign_view
                            campaign_loading
                            encounter_pending
                            encounter_notice
                        />

                        <p
                            class="encounter-notice"
                            role="status"
                            aria-live="polite"
                            aria-busy=move || encounter_pending.get()
                        >
                            {move || if encounter_pending.get() {
                                "Resolving deterministic mechanics and saving…".to_owned()
                            } else {
                                encounter_notice.get()
                            }}
                        </p>
                        <Show when=move || encounter_retry.get().is_some()>
                            <button
                                class="refresh-button"
                                disabled=move || encounter_pending.get()
                                on:click=move |_| {
                                    submit_encounter_intent(ui, None);
                                }
                            >
                                "Retry this exact command"
                            </button>
                        </Show>
                        <Show when=move || npc_retry.get().is_some()>
                            <button
                                class="refresh-button npc-retry-action"
                                disabled=move || encounter_pending.get()
                                on:click=move |_| {
                                    submit_npc_advance(ui, true);
                                }
                            >
                                "Retry this exact NPC advance"
                            </button>
                        </Show>
                        <button
                            class="refresh-button"
                            disabled=move || campaign_loading.get() || encounter_pending.get()
                            on:click=move |_| {
                                encounter_retry.set(None);
                                npc_retry.set(None);
                                encounter_notice.set("Reloading the saved encounter…".to_owned());
                                load_campaign_into(campaign_view, campaign_loading, roll_summary);
                            }
                        >
                            "Reload authoritative encounter"
                        </button>

                        <Show when=move || has_roll_explanations>
                            <details class="roll-explanation">
                                <summary>"Why this result?"</summary>
                                <ol>
                                    {roll_explanations
                                        .clone()
                                        .into_iter()
                                        .map(|explanation| view! { <li>{explanation}</li> })
                                        .collect_view()}
                                </ol>
                            </details>
                        </Show>
                    </div>
                }
                .into_any()
            }}
        </article>
    }
}

#[component]
fn CombatantCard(combatant: manchester_dnd_core::encounter::CombatantState) -> impl IntoView {
    let effects = if combatant.status_effects.is_empty() {
        "None".to_owned()
    } else {
        combatant
            .status_effects
            .iter()
            .map(|effect| format!("{effect:?}"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    view! {
        <section class="combatant-card" aria-label=format!("{} state", combatant.name)>
            <h3>{combatant.name.clone()}</h3>
            <p class="hp-readout">
                "HP " {combatant.hit_points.current} "/" {combatant.hit_points.maximum}
                {if combatant.hit_points.temporary > 0 {
                    format!(" + {} temporary", combatant.hit_points.temporary)
                } else {
                    String::new()
                }}
            </p>
            <p>"AC " {combatant.armor_class} " · position " {combatant.position_feet} " ft"</p>
            <p>"State " {format!("{:?}", combatant.life_status)} " · effects " {effects}</p>
        </section>
    }
}

fn submit_encounter_intent(ui: EncounterUiSignals, intent: Option<EncounterIntent>) {
    let EncounterUiSignals {
        campaign_view,
        campaign_loading,
        roll_summary,
        encounter_pending,
        encounter_retry,
        npc_retry,
        encounter_notice,
    } = ui;
    let command = match (intent, encounter_retry.get_untracked()) {
        (None, Some(command)) => command,
        (None, None) => {
            encounter_notice.set("There is no interrupted command to retry.".to_owned());
            return;
        }
        (Some(intent), _) => {
            let Some(view) = campaign_view.get_untracked() else {
                encounter_notice.set("The campaign is not ready yet.".to_owned());
                return;
            };
            let Some(encounter) = view.encounter.as_ref() else {
                encounter_notice.set("Resolve the rune check before starting combat.".to_owned());
                return;
            };
            CommitEncounterCommand {
                schema_version: ENCOUNTER_COMMIT_SCHEMA_VERSION,
                campaign_session_id: view.campaign_session_id.clone(),
                expected_campaign_revision: view.revision,
                command: EncounterCommand::new(
                    encounter.state.revision,
                    uuid::Uuid::new_v4().to_string(),
                    intent,
                ),
            }
        }
    };

    npc_retry.set(None);
    encounter_retry.set(Some(command.clone()));
    encounter_pending.set(true);
    encounter_notice.set("Saving the exact authoritative result…".to_owned());
    spawn_local(async move {
        match submit_encounter_action(command).await {
            Ok(EncounterCommandResponse::Committed(outcome)) => {
                let narration = apply_committed_encounter(campaign_view, *outcome);
                encounter_retry.set(None);
                encounter_notice.set(format!("Saved. {narration}"));
            }
            Ok(EncounterCommandResponse::Rejected(error)) => {
                let stale = error.code == "revision_conflict";
                encounter_notice.set(format_public_error(&error));
                if stale || !error.retryable {
                    encounter_retry.set(None);
                }
                if stale {
                    load_campaign_into(campaign_view, campaign_loading, roll_summary);
                }
            }
            Err(_) => {
                encounter_notice.set(
                    "The response was interrupted. Retry will reuse this exact command and cannot consume a second roll."
                        .to_owned(),
                );
            }
        }
        encounter_pending.set(false);
    });
}

fn submit_npc_advance(ui: EncounterUiSignals, retry: bool) {
    let EncounterUiSignals {
        campaign_view,
        campaign_loading,
        roll_summary,
        encounter_pending,
        encounter_retry,
        npc_retry,
        encounter_notice,
    } = ui;
    let command = if retry {
        let Some(command) = npc_retry.get_untracked() else {
            encounter_notice.set("There is no interrupted NPC advance to retry.".to_owned());
            return;
        };
        command
    } else {
        let Some(view) = campaign_view.get_untracked() else {
            encounter_notice.set("The campaign is not ready yet.".to_owned());
            return;
        };
        let Some(encounter) = view.encounter.as_ref() else {
            encounter_notice.set("Resolve the rune check before starting combat.".to_owned());
            return;
        };
        if encounter.state.status != EncounterStatus::Active
            || encounter.state.current_actor_id.as_deref()
                != Some(encounter.state.creature.id.as_str())
        {
            encounter_notice.set("The Soot Wight is not the current actor.".to_owned());
            return;
        }
        AdvanceNpcTurnCommand {
            schema_version: ADVANCE_NPC_TURN_SCHEMA_VERSION,
            campaign_session_id: view.campaign_session_id.clone(),
            expected_campaign_revision: view.revision,
            expected_encounter_revision: encounter.state.revision,
            idempotency_key: uuid::Uuid::new_v4().to_string(),
        }
    };

    encounter_retry.set(None);
    npc_retry.set(Some(command.clone()));
    encounter_pending.set(true);
    encounter_notice
        .set("The server is choosing and saving the closed Soot Wight policy step…".to_owned());
    spawn_local(async move {
        match advance_npc_turn(command).await {
            Ok(EncounterCommandResponse::Committed(outcome)) => {
                let narration = apply_committed_encounter(campaign_view, *outcome);
                npc_retry.set(None);
                encounter_notice.set(format!("Saved deterministic policy step. {narration}"));
            }
            Ok(EncounterCommandResponse::Rejected(error)) => {
                let stale = matches!(
                    error.code.as_str(),
                    "revision_conflict" | "encounter_revision_conflict"
                );
                encounter_notice.set(format_public_error(&error));
                if stale || !error.retryable {
                    npc_retry.set(None);
                }
                if stale {
                    load_campaign_into(campaign_view, campaign_loading, roll_summary);
                }
            }
            Err(_) => {
                encounter_notice.set(
                    "The response was interrupted. Retry will reuse this exact NPC advance; the server will not choose or roll twice."
                        .to_owned(),
                );
            }
        }
        encounter_pending.set(false);
    });
}

fn apply_committed_encounter(
    campaign_view: RwSignal<Option<LocalCampaignViewDto>>,
    outcome: CommittedEncounterOutcomeDto,
) -> String {
    let narration = outcome.resolution.narration.authored_text.clone();
    campaign_view.update(|current| {
        if let Some(view) = current {
            view.revision = outcome.result_campaign_revision;
            view.last_event_sequence = outcome.event_sequence;
            if let Some(encounter) = &mut view.encounter {
                encounter.campaign_revision = outcome.result_campaign_revision;
                encounter.last_event_sequence = outcome.event_sequence;
                encounter.state = outcome.resolution.state.clone();
                encounter.legal_actions.clone_from(&outcome.legal_actions);
                encounter.latest_outcome = Some(outcome);
            }
        }
    });
    narration
}

fn encounter_action_spec(
    action: &LegalEncounterAction,
    state: &EncounterState,
) -> Option<(String, EncounterIntent)> {
    let pending_reaction = state
        .live_q04
        .as_ref()
        .is_some_and(|live| live.pending_attack_reaction.is_some());
    if state.status == EncounterStatus::Active
        && state.current_actor_id.as_deref() != Some(state.hero.id.as_str())
        && !pending_reaction
    {
        return None;
    }
    match action {
        LegalEncounterAction::StartEncounter => Some((
            "Roll initiative and begin".to_owned(),
            EncounterIntent::StartEncounter,
        )),
        LegalEncounterAction::Move {
            minimum_destination_feet,
            maximum_destination_feet,
            ..
        } => {
            let destination = recommended_destination(
                state,
                *minimum_destination_feet,
                *maximum_destination_feet,
            )?;
            Some((
                format!("Move to {destination} ft"),
                EncounterIntent::Move {
                    destination_feet: destination,
                },
            ))
        }
        LegalEncounterAction::Attack {
            attack_id,
            target_id,
            ..
        } => Some((
            format!(
                "Attack {}",
                if target_id == &state.hero.id {
                    state.hero.name.as_str()
                } else {
                    state.creature.name.as_str()
                }
            ),
            EncounterIntent::Attack {
                attack_id: attack_id.clone(),
                target_id: target_id.clone(),
            },
        )),
        LegalEncounterAction::ContextAction { action_id } => Some((
            "Release the sluice gate".to_owned(),
            EncounterIntent::ContextAction {
                action_id: action_id.clone(),
            },
        )),
        LegalEncounterAction::CastSpell {
            spell, target_id, ..
        } => {
            let label = match spell {
                SpellId::FireBolt => "Cast Fire Bolt",
                SpellId::MagicMissile => "Cast Magic Missile",
                SpellId::Light | SpellId::MageHand | SpellId::Shield | SpellId::Sleep => {
                    return None;
                }
            };
            Some((
                label.to_owned(),
                EncounterIntent::CastSpell {
                    spell: *spell,
                    target_id: target_id.clone(),
                },
            ))
        }
        LegalEncounterAction::CastLight { object_id } => Some((
            format!("Cast Light on {}", authored_object_label(object_id)),
            EncounterIntent::CastLight {
                object_id: object_id.clone(),
            },
        )),
        LegalEncounterAction::CastMageHand { anchor_object_id } => Some((
            format!(
                "Cast Mage Hand by {}",
                authored_object_label(anchor_object_id)
            ),
            EncounterIntent::CastMageHand {
                anchor_object_id: anchor_object_id.clone(),
            },
        )),
        LegalEncounterAction::ControlMageHand { object_id } => Some((
            format!("Use Mage Hand on {}", authored_object_label(object_id)),
            EncounterIntent::ControlMageHand {
                object_id: object_id.clone(),
            },
        )),
        LegalEncounterAction::DismissMageHand => Some((
            "Dismiss Mage Hand".to_owned(),
            EncounterIntent::DismissMageHand,
        )),
        LegalEncounterAction::CastSleep => {
            Some(("Cast Sleep".to_owned(), EncounterIntent::CastSleep))
        }
        LegalEncounterAction::CastShield => {
            Some(("React with Shield".to_owned(), EncounterIntent::CastShield))
        }
        LegalEncounterAction::DeclineReaction => Some((
            "Decline Shield and take the hit".to_owned(),
            EncounterIntent::DeclineReaction,
        )),
        LegalEncounterAction::SecondWind => {
            Some(("Use Second Wind".to_owned(), EncounterIntent::SecondWind))
        }
        LegalEncounterAction::ActionSurge => {
            Some(("Use Action Surge".to_owned(), EncounterIntent::ActionSurge))
        }
        LegalEncounterAction::BeginShortRest => Some((
            "Begin a short rest".to_owned(),
            EncounterIntent::BeginShortRest,
        )),
        LegalEncounterAction::SpendHitDie => Some((
            "Confirm: spend one hit die".to_owned(),
            EncounterIntent::SpendHitDie,
        )),
        LegalEncounterAction::UseArcaneRecovery => Some((
            "Use Arcane Recovery".to_owned(),
            EncounterIntent::UseArcaneRecovery,
        )),
        LegalEncounterAction::FinishShortRest => Some((
            "Finish the short rest".to_owned(),
            EncounterIntent::FinishShortRest,
        )),
        LegalEncounterAction::TakeLongRest => Some((
            "Take an eight-hour long rest".to_owned(),
            EncounterIntent::TakeLongRest,
        )),
        LegalEncounterAction::EndTurn => {
            Some(("End the current turn".to_owned(), EncounterIntent::EndTurn))
        }
        LegalEncounterAction::RollDeathSave => Some((
            "Roll the Canal Warden's death save".to_owned(),
            EncounterIntent::RollDeathSave,
        )),
    }
}

pub(crate) fn authored_object_label(object_id: &str) -> &'static str {
    match object_id {
        manchester_dnd_core::encounter::VIADUCT_RUNE_OBJECT_ID => "the viaduct rune stone",
        manchester_dnd_core::encounter::SLUICE_LEVER_OBJECT_ID => "the cleansing sluice lever",
        _ => "the authored object",
    }
}

fn live_rules_resource_summary(state: &EncounterState) -> String {
    let Some(rules) = &state.hero_rules else {
        return "Live class resources are unavailable for this legacy encounter.".to_owned();
    };
    match rules.runtime_resources.class {
        manchester_dnd_core::hero::HeroClass::Fighter => {
            let second_wind = rules.runtime_resources.second_wind.as_ref().map_or_else(
                || "not available".to_owned(),
                |resource| format!("{}/{}", resource.current, resource.maximum),
            );
            let action_surge = rules.runtime_resources.action_surge.as_ref().map_or_else(
                || "unavailable before level 2".to_owned(),
                |resource| format!("{}/{}", resource.current, resource.maximum),
            );
            format!(
                "Live class resources · Second Wind {second_wind} · Action Surge {action_surge}"
            )
        }
        manchester_dnd_core::hero::HeroClass::Wizard => {
            let slots = rules
                .runtime_resources
                .level_one_spell_slots
                .as_ref()
                .map_or_else(
                    || "not available".to_owned(),
                    |resource| format!("{}/{}", resource.current, resource.maximum),
                );
            format!(
                "Live spell resources · level-one slots {slots} · Fire Bolt uses no slot · Magic Missile uses one"
            )
        }
    }
}

fn recommended_destination(state: &EncounterState, minimum: u16, maximum: u16) -> Option<u16> {
    let actor = state.current_actor()?;
    if actor.id == state.hero.id
        && state.objectives.contextual.status == ObjectiveStatus::Pending
        && (minimum..=maximum).contains(&state.map.sluice_position_feet)
        && actor.position_feet != state.map.sluice_position_feet
    {
        return Some(state.map.sluice_position_feet);
    }
    let target_position = state.creature.position_feet;
    let desired = if actor.position_feet <= target_position {
        target_position.saturating_sub(5)
    } else {
        target_position.saturating_add(5)
    }
    .clamp(minimum, maximum);
    if desired != actor.position_feet {
        Some(desired)
    } else if minimum != actor.position_feet {
        Some(minimum)
    } else if maximum != actor.position_feet {
        Some(maximum)
    } else {
        None
    }
}

fn encounter_status_label(status: EncounterStatus) -> &'static str {
    match status {
        EncounterStatus::Ready => "Ready",
        EncounterStatus::Active => "Active",
        EncounterStatus::Victory => "Victory — transition saved",
        EncounterStatus::Defeat => "Defeat — recovery transition saved",
    }
}

fn availability(available: bool) -> &'static str {
    if available { "available" } else { "spent" }
}

fn format_roll_explanations(outcome: &CommittedEncounterOutcomeDto) -> Vec<String> {
    outcome
        .resolution
        .rolls
        .iter()
        .zip(&outcome.roll_records)
        .map(|(raw, record)| {
            let modifiers = if record.modifier_components.is_empty() {
                "no modifiers".to_owned()
            } else {
                record
                    .modifier_components
                    .iter()
                    .map(|modifier| format!("{} {:+}", modifier.name, modifier.value))
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            let comparison = raw.comparison.as_ref().map_or_else(String::new, |comparison| {
                format!(" against {:?} {}", comparison.kind, comparison.value)
            });
            format!(
                "{:?}: {} rolled {:?}; {modifiers}; total {}{comparison} → {:?}. Source {}. RNG {} cursor {}→{} (seed reference {}).",
                raw.purpose,
                record.expression,
                record.rolled_dice,
                record.total,
                raw.outcome,
                record.ruleset,
                record.algorithm_id,
                record.cursor_before,
                record.cursor_after,
                record.seed_reference,
            )
        })
        .collect()
}

pub(crate) fn load_campaign_into(
    campaign_view: RwSignal<Option<LocalCampaignViewDto>>,
    campaign_loading: RwSignal<bool>,
    roll_summary: RwSignal<String>,
) {
    campaign_loading.set(true);
    spawn_local(async move {
        match load_local_campaign().await {
            Ok(CampaignLoadResponse::Ready(view)) => {
                let view = *view;
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

fn format_social_outcome(outcome: &SocialInteractionOutcomeDto) -> String {
    let result = &outcome.check.result;
    let result_label = if result.outcome == D20TestOutcome::Success {
        "success"
    } else {
        "setback"
    };
    format!(
        "Saved social roll {}; Charisma {:+} + {} proficiency = {} vs mapped DC {} — {result_label}. Objective, clock, attitude, and turn {} committed at revision {}.",
        result.roll.selected,
        result.ability_modifier,
        result.proficiency_modifier,
        result.total,
        outcome.check.difficulty.difficulty_class,
        outcome.resulting_state.turn,
        outcome.result_revision,
    )
}

fn format_public_error(error: &PublicGameError) -> String {
    let alternatives = if error.alternatives.is_empty() {
        String::new()
    } else {
        format!(
            " Available alternatives: {}.",
            error
                .alternatives
                .iter()
                .map(|alternative| alternative.label.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    format!(
        "{}{} [{}; reference {}]",
        error.message, alternatives, error.code, error.correlation_id
    )
}

#[cfg(feature = "hydrate")]
fn stored_preference(key: &str) -> Option<bool> {
    web_sys::window()?
        .local_storage()
        .ok()??
        .get_item(key)
        .ok()?
        .map(|value| value == "true")
}

#[cfg(not(feature = "hydrate"))]
fn stored_preference(_key: &str) -> Option<bool> {
    None
}

#[cfg(feature = "hydrate")]
fn store_preference(key: &str, value: bool) {
    if let Some(storage) =
        web_sys::window().and_then(|window| window.local_storage().ok().flatten())
    {
        let _ = storage.set_item(key, if value { "true" } else { "false" });
    }
}

#[cfg(not(feature = "hydrate"))]
fn store_preference(_key: &str, _value: bool) {}
#[cfg(test)]
mod tests {
    use super::*;
    use manchester_dnd_core::{
        encounter::{
            EncounterHeroRulesProfile, LethalityPolicy, OpeningConsequence, SOOT_WIGHT_ID,
        },
        hero::{HeroClass, SupportedLevel},
        rules_matrix::RuntimeResources,
    };

    #[test]
    fn encounter_controls_map_only_live_spell_and_class_intents() {
        let state = EncounterState::new(
            LethalityPolicy::StoryRecovery,
            OpeningConsequence::RunesUnderstood,
        );
        assert_eq!(
            encounter_action_spec(
                &LegalEncounterAction::CastSpell {
                    spell: SpellId::MagicMissile,
                    target_id: SOOT_WIGHT_ID.to_owned(),
                    range_feet: 120,
                },
                &state,
            ),
            Some((
                "Cast Magic Missile".to_owned(),
                EncounterIntent::CastSpell {
                    spell: SpellId::MagicMissile,
                    target_id: SOOT_WIGHT_ID.to_owned(),
                },
            ))
        );
        assert!(
            encounter_action_spec(
                &LegalEncounterAction::CastSpell {
                    spell: SpellId::Shield,
                    target_id: SOOT_WIGHT_ID.to_owned(),
                    range_feet: 120,
                },
                &state,
            )
            .is_none()
        );
        assert_eq!(
            encounter_action_spec(&LegalEncounterAction::SecondWind, &state),
            Some(("Use Second Wind".to_owned(), EncounterIntent::SecondWind))
        );
        assert_eq!(
            encounter_action_spec(&LegalEncounterAction::ActionSurge, &state),
            Some(("Use Action Surge".to_owned(), EncounterIntent::ActionSurge))
        );
    }

    #[test]
    fn encounter_resource_summary_reports_only_live_class_pools() {
        let mut state = EncounterState::new(
            LethalityPolicy::StoryRecovery,
            OpeningConsequence::RunesUnderstood,
        );
        state.hero_rules = Some(EncounterHeroRulesProfile {
            runtime_resources: RuntimeResources::new(HeroClass::Fighter, SupportedLevel::Two),
            spellcasting: None,
            constitution_modifier: None,
        });
        let summary = live_rules_resource_summary(&state);
        assert!(summary.contains("Second Wind 1/1"));
        assert!(summary.contains("Action Surge 1/1"));
        assert!(!summary.contains("Arcane Recovery"));
        assert!(!summary.contains("Hit Dice"));
    }
}
