use leptos::task::spawn_local;
use leptos::{prelude::*, server_fn::codec::Json};
use leptos_router::hooks::use_query_map;
use manchester_dnd_core::LocalCampaignViewDto;
use manchester_dnd_core::hero::{
    AncestryId, ArcaneTraditionId, BackgroundId, BackgroundSelection, ClassSelection, CreationStep,
    EquipmentId, EquipmentSelection, FightingStyleId, HERO_COMMAND_SCHEMA_VERSION, HeroCharacter,
    HeroConceptId, HeroCreationCommand, HeroCreationDraft, HeroCreationIntent, HeroPins,
    HeroPresentation, HitPointGrowthChoice, LEVEL_TWO_XP, LevelUpChoice, LevelUpCommand,
    SimpleWeaponId, SkillId, SpellId, StandardArrayAssignment, ThemeId, WizardSpellSelection,
};
use serde::{Deserialize, Serialize};

use crate::campaign::PublicGameError;

const LOCAL_HERO_ID: &str = "local-hero";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HeroWorkspaceView {
    pub schema_version: u16,
    pub draft: Option<HeroCreationDraft>,
    pub character: Option<HeroCharacter>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", content = "payload", rename_all = "snake_case")]
pub enum HeroWorkspaceResponse {
    Ready(Box<HeroWorkspaceView>),
    Rejected(PublicGameError),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HeroMutationView {
    pub draft: HeroCreationDraft,
    pub character: Option<HeroCharacter>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", content = "payload", rename_all = "snake_case")]
pub enum HeroMutationResponse {
    Committed(Box<HeroMutationView>),
    Rejected(PublicGameError),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", content = "payload", rename_all = "snake_case")]
pub enum HeroCharacterResponse {
    Committed(Box<HeroCharacter>),
    Rejected(PublicGameError),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EncounterRewardIntent {
    pub schema_version: u16,
    pub campaign_session_id: String,
    pub character_id: String,
    pub expected_campaign_revision: u64,
    pub expected_character_revision: u64,
    pub idempotency_key: String,
}

#[server(input = Json)]
pub async fn claim_saved_encounter_reward(
    command: EncounterRewardIntent,
) -> Result<HeroCharacterResponse, ServerFnError> {
    #[cfg(feature = "ssr")]
    {
        use manchester_dnd_server::{ClaimEncounterRewardCommand, ServerContext};

        let headers = crate::campaign::request_headers().await;
        let correlation_id = crate::campaign::request_correlation_id(headers.as_ref());
        if headers
            .as_ref()
            .is_none_or(|headers| !crate::campaign::headers_are_same_origin(headers))
        {
            return Ok(HeroCharacterResponse::Rejected(
                crate::campaign::invalid_origin_error(correlation_id),
            ));
        }
        let Some(context) = use_context::<ServerContext>() else {
            return Ok(HeroCharacterResponse::Rejected(
                crate::campaign::internal_error(correlation_id),
            ));
        };
        let trusted = ClaimEncounterRewardCommand {
            schema_version: command.schema_version,
            campaign_session_id: command.campaign_session_id,
            character_id: command.character_id,
            expected_campaign_revision: command.expected_campaign_revision,
            expected_character_revision: command.expected_character_revision,
            idempotency_key: command.idempotency_key,
        };
        match context
            .application
            .claim_local_encounter_reward(trusted)
            .await
        {
            Ok(outcome) => Ok(HeroCharacterResponse::Committed(Box::new(
                outcome.character,
            ))),
            Err(error) => Ok(HeroCharacterResponse::Rejected(
                crate::campaign::public_error(&error, correlation_id),
            )),
        }
    }

    #[cfg(not(feature = "ssr"))]
    {
        let _ = command;
        unreachable!("the server-function macro replaces this body in browser builds")
    }
}

#[server(input = Json)]
pub async fn load_hero_workspace() -> Result<HeroWorkspaceResponse, ServerFnError> {
    #[cfg(feature = "ssr")]
    {
        use manchester_dnd_server::ServerContext;

        let headers = crate::campaign::request_headers().await;
        let correlation_id = crate::campaign::request_correlation_id(headers.as_ref());
        if headers
            .as_ref()
            .is_none_or(|headers| !crate::campaign::headers_are_same_origin(headers))
        {
            return Ok(HeroWorkspaceResponse::Rejected(
                crate::campaign::invalid_origin_error(correlation_id),
            ));
        }
        let Some(context) = use_context::<ServerContext>() else {
            return Ok(HeroWorkspaceResponse::Rejected(
                crate::campaign::internal_error(correlation_id),
            ));
        };
        match context.application.load_local_hero_workspace().await {
            Ok(workspace) => Ok(HeroWorkspaceResponse::Ready(Box::new(HeroWorkspaceView {
                schema_version: workspace.schema_version,
                draft: workspace.draft,
                character: workspace.character,
            }))),
            Err(error) => Ok(HeroWorkspaceResponse::Rejected(
                crate::campaign::public_error(&error, correlation_id),
            )),
        }
    }

    #[cfg(not(feature = "ssr"))]
    {
        unreachable!("the server-function macro replaces this body in browser builds")
    }
}

#[server(input = Json)]
pub async fn begin_hero_creation() -> Result<HeroMutationResponse, ServerFnError> {
    #[cfg(feature = "ssr")]
    {
        use manchester_dnd_server::ServerContext;

        let headers = crate::campaign::request_headers().await;
        let correlation_id = crate::campaign::request_correlation_id(headers.as_ref());
        if headers
            .as_ref()
            .is_none_or(|headers| !crate::campaign::headers_are_same_origin(headers))
        {
            return Ok(HeroMutationResponse::Rejected(
                crate::campaign::invalid_origin_error(correlation_id),
            ));
        }
        let Some(context) = use_context::<ServerContext>() else {
            return Ok(HeroMutationResponse::Rejected(
                crate::campaign::internal_error(correlation_id),
            ));
        };
        match context.application.start_local_hero_creation().await {
            Ok(draft) => Ok(HeroMutationResponse::Committed(Box::new(
                HeroMutationView {
                    draft,
                    character: None,
                },
            ))),
            Err(error) => Ok(HeroMutationResponse::Rejected(
                crate::campaign::public_error(&error, correlation_id),
            )),
        }
    }

    #[cfg(not(feature = "ssr"))]
    {
        unreachable!("the server-function macro replaces this body in browser builds")
    }
}

#[server(input = Json)]
pub async fn advance_hero_creation(
    command: HeroCreationCommand,
) -> Result<HeroMutationResponse, ServerFnError> {
    #[cfg(feature = "ssr")]
    {
        use manchester_dnd_server::ServerContext;

        let headers = crate::campaign::request_headers().await;
        let correlation_id = crate::campaign::request_correlation_id(headers.as_ref());
        if headers
            .as_ref()
            .is_none_or(|headers| !crate::campaign::headers_are_same_origin(headers))
        {
            return Ok(HeroMutationResponse::Rejected(
                crate::campaign::invalid_origin_error(correlation_id),
            ));
        }
        let Some(context) = use_context::<ServerContext>() else {
            return Ok(HeroMutationResponse::Rejected(
                crate::campaign::internal_error(correlation_id),
            ));
        };
        let draft_id = command.draft_id.clone();
        match context
            .application
            .apply_hero_creation_command(command)
            .await
        {
            Ok(outcome) => match context
                .application
                .load_local_hero_creation(&draft_id)
                .await
            {
                Ok(draft) => Ok(HeroMutationResponse::Committed(Box::new(
                    HeroMutationView {
                        draft,
                        character: outcome.character,
                    },
                ))),
                Err(error) => Ok(HeroMutationResponse::Rejected(
                    crate::campaign::public_error(&error, correlation_id),
                )),
            },
            Err(error) => Ok(HeroMutationResponse::Rejected(
                crate::campaign::public_error(&error, correlation_id),
            )),
        }
    }

    #[cfg(not(feature = "ssr"))]
    {
        let _ = command;
        unreachable!("the server-function macro replaces this body in browser builds")
    }
}

#[server(input = Json)]
pub async fn advance_hero_level(
    command: LevelUpCommand,
) -> Result<HeroCharacterResponse, ServerFnError> {
    #[cfg(feature = "ssr")]
    {
        use manchester_dnd_server::ServerContext;

        let headers = crate::campaign::request_headers().await;
        let correlation_id = crate::campaign::request_correlation_id(headers.as_ref());
        if headers
            .as_ref()
            .is_none_or(|headers| !crate::campaign::headers_are_same_origin(headers))
        {
            return Ok(HeroCharacterResponse::Rejected(
                crate::campaign::invalid_origin_error(correlation_id),
            ));
        }
        let Some(context) = use_context::<ServerContext>() else {
            return Ok(HeroCharacterResponse::Rejected(
                crate::campaign::internal_error(correlation_id),
            ));
        };
        match context.application.level_up_hero(command).await {
            Ok(outcome) => Ok(HeroCharacterResponse::Committed(Box::new(
                outcome.character,
            ))),
            Err(error) => Ok(HeroCharacterResponse::Rejected(
                crate::campaign::public_error(&error, correlation_id),
            )),
        }
    }

    #[cfg(not(feature = "ssr"))]
    {
        let _ = command;
        unreachable!("the server-function macro replaces this body in browser builds")
    }
}

#[component]
pub fn HeroCreator(
    campaign_view: RwSignal<Option<LocalCampaignViewDto>>,
    campaign_loading: RwSignal<bool>,
    campaign_notice: RwSignal<String>,
) -> impl IntoView {
    let query = use_query_map();
    let preview_theme = RwSignal::new(theme_from_query(
        query.get_untracked().get("theme").as_deref(),
    ));
    let draft = RwSignal::new(None::<HeroCreationDraft>);
    let character = RwSignal::new(None::<HeroCharacter>);
    let loading = RwSignal::new(true);
    let pending = RwSignal::new(false);
    let retry = RwSignal::new(None::<HeroCreationCommand>);
    let reward_retry = RwSignal::new(None::<EncounterRewardIntent>);
    let level_retry = RwSignal::new(None::<LevelUpCommand>);
    let notice = RwSignal::new(String::from("Loading your saved hero workspace…"));
    let observed_campaign_revision = RwSignal::new(None::<u64>);

    let name = RwSignal::new(String::from("Mara Vale"));
    let pronouns = RwSignal::new(String::from("they/them"));
    let appearance = RwSignal::new(String::from(
        "A rain-dark coat, a brass lantern, and a watchful expression.",
    ));
    let ideal = RwSignal::new(String::from("No one is abandoned beneath the city."));
    let bond = RwSignal::new(String::from("The canal wards sheltered my family."));
    let flaw = RwSignal::new(String::from(
        "I take every warning as a personal challenge.",
    ));
    let tone_limits = RwSignal::new(String::from("No graphic gore"));
    let campaign_refresh = CampaignRefresh {
        view: campaign_view,
        loading: campaign_loading,
        notice: campaign_notice,
    };

    Effect::new(move |_| load_workspace_into(draft, character, loading, notice));
    Effect::new(move |_| {
        let revision = campaign_view.get().map(|view| view.revision);
        if revision != observed_campaign_revision.get_untracked() {
            observed_campaign_revision.set(revision);
            if revision.is_some() {
                refresh_character_into(character);
            }
        }
    });

    view! {
        <article class="panel theme-panel hero-creator" id="hero-creator" aria-labelledby="forge-heading">
            <div class="panel-heading">
                <div>
                    <p class="eyebrow">"CHARACTER FORGE"</p>
                    <h2 id="forge-heading">"Create a rules-valid hero"</h2>
                </div>
                <span class="step">
                    {move || draft.get().map_or_else(
                        || if character.get().is_some() { "COMPLETE".to_owned() } else { "READY".to_owned() },
                        |draft| step_label(draft.step),
                    )}
                </span>
            </div>

            <p class="hero-save-state" role="status" aria-live="polite" aria-busy=move || loading.get() || pending.get()>
                {move || notice.get()}
            </p>

            {move || {
                if loading.get() {
                    return view! { <div class="hero-empty"><p>"Loading the authoritative draft…"</p></div> }.into_any();
                }
                if let Some(hero) = character.get() {
                    return view! {
                        <CreatedHero
                            hero
                            character
                            pending
                            reward_retry
                            level_retry
                            notice
                        />
                    }.into_any();
                }
                let Some(current) = draft.get() else {
                    return view! {
                        <div class="hero-empty">
                            <p>"Your choices are checked and saved after every step. A draft lasts seven days, followed by a thirty-day deletion window."</p>
                            <button
                                class="primary-button"
                                disabled=move || pending.get()
                                on:click=move |_| begin_creation(draft, character, pending, retry, notice)
                            >
                                "Begin guided creation"
                            </button>
                        </div>
                    }.into_any();
                };

                let step = current.step;
                view! {
                    <div class="hero-step" data-step=format!("{step:?}")>
                        <HeroDraftStep
                            current
                            draft
                            character
                            pending
                            retry
                            notice
                            name
                            pronouns
                            appearance
                            ideal
                            bond
                            flaw
                            tone_limits
                            campaign_refresh
                        />
                    </div>
                }.into_any()
            }}

            <Show when=move || retry.get().is_some()>
                <button
                    class="refresh-button"
                    disabled=move || pending.get()
                    on:click=move |_| submit_creation_intent(
                        draft,
                        character,
                        pending,
                        retry,
                        notice,
                        None,
                        campaign_refresh,
                    )
                >
                    "Retry the exact saved choice"
                </button>
            </Show>
            <button
                class="refresh-button"
                disabled=move || pending.get() || loading.get()
                on:click=move |_| {
                    retry.set(None);
                    reward_retry.set(None);
                    level_retry.set(None);
                    notice.set("Reloading the authoritative hero workspace…".to_owned());
                    load_workspace_into(draft, character, loading, notice);
                    campaign_refresh.reload();
                }
            >
                "Reload saved hero"
            </button>

            <details class="native-theme-preview">
                <summary>"Preview themes without JavaScript"</summary>
                <p>"This presentation-only preview is not a saved choice. The server validates the theme after creation begins."</p>
                <form method="get" action="/#hero-creator">
                    <fieldset>
                        <legend>"Theme preview"</legend>
                        <ThemePreviewOption
                            value="rainbound-borough"
                            name="Rainbound Borough"
                            detail="Canals, rain-slick wards, and lantern oaths"
                            preview_theme
                        />
                        <ThemePreviewOption
                            value="emberline-archive"
                            name="Emberline Archive"
                            detail="Hidden stacks, warm brass, and living records"
                            preview_theme
                        />
                    </fieldset>
                    <button class="preview-button" type="submit">"Preview selected theme"</button>
                </form>
                <p class="selection-copy">"Previewing: " <strong>{move || preview_theme.get()}</strong></p>
            </details>
        </article>
    }
}

#[component]
fn ThemePreviewOption(
    value: &'static str,
    name: &'static str,
    detail: &'static str,
    preview_theme: RwSignal<&'static str>,
) -> impl IntoView {
    view! {
        <label class="theme-button" class:selected=move || preview_theme.get() == name>
            <input
                type="radio"
                name="theme"
                value=value
                checked=move || preview_theme.get() == name
                on:change=move |_| preview_theme.set(name)
            />
            <span class="theme-sigil">{name.chars().next().unwrap_or('M')}</span>
            <span><strong>{name}</strong><small>{detail}</small></span>
            <span class="theme-arrow">"↗"</span>
        </label>
    }
}

#[allow(clippy::too_many_arguments)]
#[component]
fn HeroDraftStep(
    current: HeroCreationDraft,
    draft: RwSignal<Option<HeroCreationDraft>>,
    character: RwSignal<Option<HeroCharacter>>,
    pending: RwSignal<bool>,
    retry: RwSignal<Option<HeroCreationCommand>>,
    notice: RwSignal<String>,
    name: RwSignal<String>,
    pronouns: RwSignal<String>,
    appearance: RwSignal<String>,
    ideal: RwSignal<String>,
    bond: RwSignal<String>,
    flaw: RwSignal<String>,
    tone_limits: RwSignal<String>,
    campaign_refresh: CampaignRefresh,
) -> impl IntoView {
    let send = move |intent| {
        submit_creation_intent(
            draft,
            character,
            pending,
            retry,
            notice,
            Some(intent),
            campaign_refresh,
        );
    };

    match current.step {
        CreationStep::CampaignTheme => view! {
            <fieldset class="hero-fieldset">
                <legend>"1. Choose a presentation theme"</legend>
                <p>"Both original themes use the identical pinned rules package."</p>
                <div class="hero-choice-grid">
                    <button disabled=move || pending.get() on:click=move |_| send(HeroCreationIntent::SelectCampaignTheme { pins: HeroPins::mvp(ThemeId::RainboundBorough) })>
                        <strong>"Rainbound Borough"</strong><span>"Canals, rain-slick wards, and lantern oaths"</span>
                    </button>
                    <button disabled=move || pending.get() on:click=move |_| send(HeroCreationIntent::SelectCampaignTheme { pins: HeroPins::mvp(ThemeId::EmberlineArchive) })>
                        <strong>"Emberline Archive"</strong><span>"Hidden stacks, warm brass, and living records"</span>
                    </button>
                </div>
            </fieldset>
        }.into_any(),
        CreationStep::Concept => view! {
            <fieldset class="hero-fieldset">
                <legend>"2. Choose an original story concept"</legend>
                <div class="hero-choice-grid">
                    <button disabled=move || pending.get() on:click=move |_| send(HeroCreationIntent::SelectConcept { concept: HeroConceptId::CanalGuardian })>
                        <strong>"Canal Guardian"</strong><span>"Protect neighbours and forgotten crossings"</span>
                    </button>
                    <button disabled=move || pending.get() on:click=move |_| send(HeroCreationIntent::SelectConcept { concept: HeroConceptId::ArchiveSeeker })>
                        <strong>"Archive Seeker"</strong><span>"Recover truths the city tried to bury"</span>
                    </button>
                </div>
            </fieldset>
        }.into_any(),
        CreationStep::Rules => view! {
            <fieldset class="hero-fieldset">
                <legend>"3. Choose the supported human class path"</legend>
                <div class="hero-choice-grid">
                    <button disabled=move || pending.get() on:click=move |_| send(HeroCreationIntent::SelectRules { ancestry: AncestryId::Human, class: ClassSelection::Fighter { fighting_style: FightingStyleId::Defense } })>
                        <strong>"Human Fighter · Defense"</strong><span>"Armoured guardian; Second Wind at level 1"</span>
                    </button>
                    <button disabled=move || pending.get() on:click=move |_| send(HeroCreationIntent::SelectRules { ancestry: AncestryId::Human, class: ClassSelection::Fighter { fighting_style: FightingStyleId::Dueling } })>
                        <strong>"Human Fighter · Dueling"</strong><span>"One-handed weapon specialist"</span>
                    </button>
                    <button disabled=move || pending.get() on:click=move |_| send(HeroCreationIntent::SelectRules { ancestry: AncestryId::Human, class: ClassSelection::Wizard })>
                        <strong>"Human Wizard"</strong><span>"Six allowlisted spells; Fire Bolt and Magic Missile are live in this encounter"</span>
                    </button>
                </div>
            </fieldset>
        }.into_any(),
        CreationStep::AbilityScores => {
            let is_wizard = matches!(current.class, Some(ClassSelection::Wizard));
            view! {
                <fieldset class="hero-fieldset">
                    <legend>"4. Assign the fixed standard array"</legend>
                    <p>"Each preset uses 15, 14, 13, 12, 10, and 8 exactly once; human adjustments are derived by the rules engine."</p>
                    <div class="hero-choice-grid">
                        {if is_wizard {
                            view! {
                                <button disabled=move || pending.get() on:click=move |_| send(HeroCreationIntent::AssignAbilities { assignment: wizard_assignment(false) })>
                                    <strong>"Scholarly"</strong><span>"STR 8 · DEX 14 · CON 13 · INT 15 · WIS 12 · CHA 10"</span>
                                </button>
                                <button disabled=move || pending.get() on:click=move |_| send(HeroCreationIntent::AssignAbilities { assignment: wizard_assignment(true) })>
                                    <strong>"Resolute"</strong><span>"STR 8 · DEX 13 · CON 14 · INT 15 · WIS 12 · CHA 10"</span>
                                </button>
                            }.into_any()
                        } else {
                            view! {
                                <button disabled=move || pending.get() on:click=move |_| send(HeroCreationIntent::AssignAbilities { assignment: fighter_assignment(false) })>
                                    <strong>"Steadfast"</strong><span>"STR 15 · DEX 12 · CON 14 · INT 8 · WIS 13 · CHA 10"</span>
                                </button>
                                <button disabled=move || pending.get() on:click=move |_| send(HeroCreationIntent::AssignAbilities { assignment: fighter_assignment(true) })>
                                    <strong>"Agile"</strong><span>"STR 13 · DEX 15 · CON 14 · INT 8 · WIS 12 · CHA 10"</span>
                                </button>
                            }.into_any()
                        }}
                    </div>
                </fieldset>
            }.into_any()
        }
        CreationStep::Background => {
            let class = current.class.as_ref().expect("validated draft has a class").class();
            view! {
                <fieldset class="hero-fieldset">
                    <legend>"5. Choose a background and valid proficiencies"</legend>
                    <div class="hero-choice-grid">
                        <button disabled=move || pending.get() on:click=move |_| send(HeroCreationIntent::SelectBackground { selection: background_selection(class, BackgroundId::Soldier) })>
                            <strong>"Soldier"</strong><span>{background_detail(class, BackgroundId::Soldier)}</span>
                        </button>
                        <button disabled=move || pending.get() on:click=move |_| send(HeroCreationIntent::SelectBackground { selection: background_selection(class, BackgroundId::Sage) })>
                            <strong>"Sage"</strong><span>{background_detail(class, BackgroundId::Sage)}</span>
                        </button>
                    </div>
                </fieldset>
            }.into_any()
        }
        CreationStep::EquipmentAndSpells => {
            let is_wizard = matches!(current.class, Some(ClassSelection::Wizard));
            view! {
                <fieldset class="hero-fieldset">
                    <legend>"6. Choose a validated starting loadout"</legend>
                    <div class="hero-choice-grid">
                        {if is_wizard {
                            view! {
                                <button disabled=move || pending.get() on:click=move |_| send(HeroCreationIntent::SelectEquipmentAndSpells { equipment: wizard_equipment(SimpleWeaponId::Dagger), wizard_spells: Some(fixed_wizard_spells()) })>
                                    <strong>"Dagger and focus"</strong><span>"Scholar's pack, spellbook, focus, and six allowlisted spells; two are currently playable"</span>
                                </button>
                                <button disabled=move || pending.get() on:click=move |_| send(HeroCreationIntent::SelectEquipmentAndSpells { equipment: wizard_equipment(SimpleWeaponId::Quarterstaff), wizard_spells: Some(fixed_wizard_spells()) })>
                                    <strong>"Quarterstaff and focus"</strong><span>"Scholar's pack, spellbook, focus, and six allowlisted spells; two are currently playable"</span>
                                </button>
                            }.into_any()
                        } else {
                            view! {
                                <button disabled=move || pending.get() on:click=move |_| send(HeroCreationIntent::SelectEquipmentAndSpells { equipment: guard_equipment(), wizard_spells: None })>
                                    <strong>"Canal guard"</strong><span>"Chain mail, shield, longsword, crossbow, and explorer's pack"</span>
                                </button>
                                <button disabled=move || pending.get() on:click=move |_| send(HeroCreationIntent::SelectEquipmentAndSpells { equipment: scout_equipment(), wizard_spells: None })>
                                    <strong>"Viaduct scout"</strong><span>"Leather armour, spear, crossbow, and explorer's pack"</span>
                                </button>
                            }.into_any()
                        }}
                    </div>
                </fieldset>
            }.into_any()
        }
        CreationStep::Presentation => view! {
            <fieldset class="hero-fieldset hero-presentation">
                <legend>"7. Describe the person, not new mechanics"</legend>
                <label>"Name"<input required maxlength="80" prop:value=move || name.get() on:input=move |event| name.set(event_target_value(&event))/></label>
                <label>"Pronouns"<input required maxlength="60" prop:value=move || pronouns.get() on:input=move |event| pronouns.set(event_target_value(&event))/></label>
                <label>"Appearance"<textarea required maxlength="500" rows="3" prop:value=move || appearance.get() on:input=move |event| appearance.set(event_target_value(&event))></textarea></label>
                <label>"Ideal"<textarea required maxlength="500" rows="2" prop:value=move || ideal.get() on:input=move |event| ideal.set(event_target_value(&event))></textarea></label>
                <label>"Bond"<textarea required maxlength="500" rows="2" prop:value=move || bond.get() on:input=move |event| bond.set(event_target_value(&event))></textarea></label>
                <label>"Flaw"<textarea required maxlength="500" rows="2" prop:value=move || flaw.get() on:input=move |event| flaw.set(event_target_value(&event))></textarea></label>
                <label>"Tone limits (comma-separated)"<input maxlength="120" prop:value=move || tone_limits.get() on:input=move |event| tone_limits.set(event_target_value(&event))/></label>
                <p>"Presentation text cannot alter AC, HP, proficiencies, equipment, spells, or legal actions."</p>
                <button
                    class="primary-button"
                    disabled=move || pending.get() || [name.get(), pronouns.get(), appearance.get(), ideal.get(), bond.get(), flaw.get()].iter().any(|value| value.trim().is_empty())
                    on:click=move |_| send(HeroCreationIntent::SetPresentation {
                        presentation: HeroPresentation {
                            name: name.get_untracked().trim().to_owned(),
                            pronouns: pronouns.get_untracked().trim().to_owned(),
                            appearance: appearance.get_untracked().trim().to_owned(),
                            ideal: ideal.get_untracked().trim().to_owned(),
                            bond: bond.get_untracked().trim().to_owned(),
                            flaw: flaw.get_untracked().trim().to_owned(),
                            tone_limits: parsed_tone_limits(&tone_limits.get_untracked()),
                        },
                    })
                >"Save presentation"</button>
            </fieldset>
        }.into_any(),
        CreationStep::Review => view! {
            <div class="hero-review">
                <h3>"Review derived choices"</h3>
                <HeroDraftSummary draft=current.clone()/>
                <p>"Live encounter scope: weapon attacks, Fighter Second Wind, level-2 Action Surge, Wizard Fire Bolt, and Wizard Magic Missile. Light, Mage Hand, Shield, Sleep, rests, hit-die spending, and Arcane Recovery remain unavailable in play."</p>
                <p class="source-label">"Rules: SRD 5.1 CC BY 4.0 subset · immutable core and theme digests shown below"</p>
                <button class="primary-button" disabled=move || pending.get() on:click=move |_| send(HeroCreationIntent::Review)>"Confirm this review"</button>
            </div>
        }.into_any(),
        CreationStep::Commit => view! {
            <div class="hero-review">
                <h3>"Commit the authoritative character"</h3>
                <HeroDraftSummary draft=current.clone()/>
                <p>"This writes CharacterCreated, every explicit choice, the exact pack pins, derived sheet version, and initial resources in one transaction."</p>
                <button class="primary-button" disabled=move || pending.get() on:click=move |_| send(HeroCreationIntent::Commit { character_id: LOCAL_HERO_ID.to_owned() })>"Create and save hero"</button>
            </div>
        }.into_any(),
        CreationStep::Committed => view! {
            <div class="hero-empty"><p>"This draft is committed. Reloading the workspace will show the authoritative character."</p></div>
        }.into_any(),
    }
}

#[component]
fn HeroDraftSummary(draft: HeroCreationDraft) -> impl IntoView {
    let pins = draft.pins.as_ref();
    view! {
        <dl class="hero-summary">
            <div><dt>"Name"</dt><dd>{draft.presentation.as_ref().map_or("—", |value| value.name.as_str()).to_owned()}</dd></div>
            <div><dt>"Theme"</dt><dd>{pins.map_or_else(|| "—".to_owned(), |pins| theme_label(pins.theme_id).to_owned())}</dd></div>
            <div><dt>"Concept"</dt><dd>{format_optional(draft.concept)}</dd></div>
            <div><dt>"Ancestry / class"</dt><dd>{format!("{:?} / {}", draft.ancestry.unwrap_or(AncestryId::Human), class_selection_label(draft.class.as_ref()))}</dd></div>
            <div><dt>"Background"</dt><dd>{draft.background.as_ref().map_or_else(|| "—".to_owned(), |value| format!("{:?}", value.background))}</dd></div>
            <div><dt>"Draft revision"</dt><dd>{draft.revision}</dd></div>
            <div><dt>"Rules pin"</dt><dd>{pins.map_or("—", |pins| pins.ruleset_id.as_str()).to_owned()}</dd></div>
            <div><dt>"Core digest"</dt><dd class="digest">{pins.map_or("—", |pins| pins.core_content.digest.as_str()).to_owned()}</dd></div>
            <div><dt>"Theme digest"</dt><dd class="digest">{pins.map_or("—", |pins| pins.theme.digest.as_str()).to_owned()}</dd></div>
        </dl>
    }
}

#[component]
fn CreatedHero(
    hero: HeroCharacter,
    character: RwSignal<Option<HeroCharacter>>,
    pending: RwSignal<bool>,
    reward_retry: RwSignal<Option<EncounterRewardIntent>>,
    level_retry: RwSignal<Option<LevelUpCommand>>,
    notice: RwSignal<String>,
) -> impl IntoView {
    let level = hero.level.value();
    let class = hero.choices.class.class();
    let eligible = hero.level_up_eligible();
    let preview = match class {
        manchester_dnd_core::hero::HeroClass::Fighter => {
            "Level 2: fixed-average HP growth and Action Surge"
        }
        manchester_dnd_core::hero::HeroClass::Wizard => {
            "Level 2: fixed-average HP growth; Evocation is recorded but dormant, and Arcane Recovery is not yet exposed in play"
        }
    };
    let attacks = hero
        .sheet
        .attacks
        .iter()
        .map(|attack| {
            format!(
                "{} {:+} · {}d{}{:+}",
                attack.attack_id,
                attack.attack_bonus,
                attack.damage.count,
                attack.damage.sides,
                attack.damage.constant
            )
        })
        .collect::<Vec<_>>()
        .join("; ");
    let actions = live_encounter_action_preview(class, level);
    let resources = hero
        .sheet
        .resources
        .iter()
        .map(|resource| {
            format!(
                "{:?} {}/{}",
                resource.resource, resource.current, resource.maximum
            )
        })
        .collect::<Vec<_>>()
        .join("; ");
    let level_command = StoredValue::new(level_up_command(&hero));

    view! {
        <div class="created-hero">
            <div class="created-hero-heading">
                <div><p class="eyebrow">"SAVED HERO"</p><h3>{hero.choices.presentation.name.clone()}</h3></div>
                <span class="level-badge">"Level " {level} " " {format!("{class:?}")}</span>
            </div>
            <p>{hero.choices.presentation.pronouns.clone()} " · " {hero.choices.presentation.appearance.clone()}</p>
            <dl class="hero-sheet">
                <div><dt>"HP"</dt><dd>{hero.sheet.current_hit_points} "/" {hero.sheet.maximum_hit_points}</dd></div>
                <div><dt>"AC"</dt><dd>{hero.sheet.armor_class}</dd></div>
                <div><dt>"Speed"</dt><dd>{hero.sheet.speed_feet} " ft"</dd></div>
                <div><dt>"Proficiency"</dt><dd>"+" {hero.sheet.proficiency_bonus}</dd></div>
                <div><dt>"Passive Perception"</dt><dd>{hero.sheet.passive_values.perception}</dd></div>
                <div><dt>"XP"</dt><dd>{hero.experience_points} "/" {LEVEL_TWO_XP}</dd></div>
                <div class="wide"><dt>"Attacks"</dt><dd>{attacks}</dd></div>
                <div class="wide"><dt>"Resources"</dt><dd>{resources}</dd></div>
                <div class="wide"><dt>"Live encounter actions"</dt><dd>{actions}</dd></div>
            </dl>
            <details class="hero-provenance">
                <summary>"Choices, provenance, and limitations"</summary>
                <p>"Rules " {hero.choices.pins.ruleset_id.as_str()} " · derivation " {hero.sheet.derivation_id.clone()}</p>
                <p class="digest">"Core " {hero.choices.pins.core_content.digest.as_str().to_owned()}</p>
                <p class="digest">"Theme " {hero.choices.pins.theme.digest.as_str().to_owned()}</p>
                <p>"Supported live scope: weapon attacks; Second Wind; Action Surge for a level-2 encounter snapshot; Fire Bolt; Magic Missile. Deferred: Light, Mage Hand, Shield, Sleep, generic core actions, rests, hit-die spending, and Arcane Recovery. Presentation grants no mechanics."</p>
            </details>
            <div class="level-preview">
                <strong>{preview}</strong>
                <p>{if eligible { "The trusted XP threshold is met." } else if level >= 2 { "The MVP level path is complete." } else { "Complete the authored encounter to earn trusted XP." }}</p>
                <Show when=move || level == 1 && !eligible>
                    <button
                        class="primary-button reward-claim-button"
                        disabled=move || pending.get()
                        on:click=move |_| claim_victory_reward(
                            character,
                            pending,
                            reward_retry,
                            notice,
                            None,
                        )
                    >"Claim completed encounter XP"</button>
                </Show>
                <Show when=move || eligible>
                    <button
                        class="primary-button"
                        disabled=move || pending.get()
                        on:click=move |_| submit_level_up(character, pending, level_retry, notice, level_command.get_value())
                    >"Apply validated level-up"</button>
                </Show>
            </div>
            <Show when=move || reward_retry.get().is_some()>
                <button class="refresh-button" disabled=move || pending.get() on:click=move |_| {
                    claim_victory_reward(
                        character,
                        pending,
                        reward_retry,
                        notice,
                        reward_retry.get_untracked(),
                    );
                }>"Retry the exact reward claim"</button>
            </Show>
            <Show when=move || level_retry.get().is_some()>
                <button class="refresh-button" disabled=move || pending.get() on:click=move |_| {
                    if let Some(command) = level_retry.get_untracked() {
                        submit_level_up(character, pending, level_retry, notice, command);
                    }
                }>"Retry the exact level-up"</button>
            </Show>
        </div>
    }
}

fn claim_victory_reward(
    character: RwSignal<Option<HeroCharacter>>,
    pending: RwSignal<bool>,
    retry: RwSignal<Option<EncounterRewardIntent>>,
    notice: RwSignal<String>,
    existing: Option<EncounterRewardIntent>,
) {
    if character.get_untracked().is_none() {
        notice.set("Reload the authoritative hero before claiming a reward.".to_owned());
        return;
    }
    pending.set(true);
    notice.set("Checking the committed encounter victory and trusted reward policy…".to_owned());
    spawn_local(async move {
        let command = if let Some(command) = existing {
            command
        } else {
            let hero = match load_hero_workspace().await {
                Ok(HeroWorkspaceResponse::Ready(workspace)) => {
                    let Some(hero) = workspace.character else {
                        notice.set("The authoritative hero no longer exists.".to_owned());
                        pending.set(false);
                        return;
                    };
                    character.set(Some(hero.clone()));
                    hero
                }
                Ok(HeroWorkspaceResponse::Rejected(error)) => {
                    notice.set(format_public_error(&error));
                    pending.set(false);
                    return;
                }
                Err(_) => {
                    notice.set(
                        "The authoritative hero could not be refreshed. No reward changed."
                            .to_owned(),
                    );
                    pending.set(false);
                    return;
                }
            };
            let campaign = match crate::campaign::load_local_campaign().await {
                Ok(crate::campaign::CampaignLoadResponse::Ready(campaign)) => *campaign,
                Ok(crate::campaign::CampaignLoadResponse::Rejected(error)) => {
                    notice.set(format_public_error(&error));
                    pending.set(false);
                    return;
                }
                Err(_) => {
                    notice.set(
                        "The saved campaign could not be loaded. No reward changed.".to_owned(),
                    );
                    pending.set(false);
                    return;
                }
            };
            EncounterRewardIntent {
                schema_version: 1,
                campaign_session_id: campaign.campaign_session_id,
                character_id: hero.character_id.clone(),
                expected_campaign_revision: campaign.revision,
                expected_character_revision: hero.revision,
                idempotency_key: uuid::Uuid::new_v4().to_string(),
            }
        };
        retry.set(Some(command.clone()));
        match claim_saved_encounter_reward(command).await {
            Ok(HeroCharacterResponse::Committed(updated)) => {
                character.set(Some(*updated));
                retry.set(None);
                notice.set("Victory reward saved. Level 2 is now available.".to_owned());
            }
            Ok(HeroCharacterResponse::Rejected(error)) => {
                notice.set(format_public_error(&error));
                if !error.retryable {
                    retry.set(None);
                }
            }
            Err(_) => notice.set(
                "The response was interrupted. Retry reuses the same reward claim and cannot award XP twice."
                    .to_owned(),
            ),
        }
        pending.set(false);
    });
}

fn refresh_character_into(character: RwSignal<Option<HeroCharacter>>) {
    spawn_local(async move {
        let Ok(HeroWorkspaceResponse::Ready(workspace)) = load_hero_workspace().await else {
            return;
        };
        let Some(refreshed) = workspace.character else {
            return;
        };
        let is_newer = character
            .get_untracked()
            .as_ref()
            .is_none_or(|current| refreshed.revision >= current.revision);
        if is_newer {
            character.set(Some(refreshed));
        }
    });
}

fn load_workspace_into(
    draft: RwSignal<Option<HeroCreationDraft>>,
    character: RwSignal<Option<HeroCharacter>>,
    loading: RwSignal<bool>,
    notice: RwSignal<String>,
) {
    loading.set(true);
    spawn_local(async move {
        match load_hero_workspace().await {
            Ok(HeroWorkspaceResponse::Ready(workspace)) => {
                let workspace = *workspace;
                draft.set(workspace.draft);
                character.set(workspace.character);
                notice.set("Authoritative hero workspace loaded.".to_owned());
            }
            Ok(HeroWorkspaceResponse::Rejected(error)) => {
                notice.set(format_public_error(&error));
            }
            Err(_) => notice.set(
                "The hero workspace could not be reached. Your saved state was not changed."
                    .to_owned(),
            ),
        }
        loading.set(false);
    });
}

fn begin_creation(
    draft: RwSignal<Option<HeroCreationDraft>>,
    character: RwSignal<Option<HeroCharacter>>,
    pending: RwSignal<bool>,
    retry: RwSignal<Option<HeroCreationCommand>>,
    notice: RwSignal<String>,
) {
    pending.set(true);
    notice.set("Creating a resumable server-owned draft…".to_owned());
    spawn_local(async move {
        match begin_hero_creation().await {
            Ok(HeroMutationResponse::Committed(result)) => {
                let result = *result;
                draft.set(Some(result.draft));
                character.set(result.character);
                retry.set(None);
                notice.set("Draft saved. Choose a theme.".to_owned());
            }
            Ok(HeroMutationResponse::Rejected(error)) => notice.set(format_public_error(&error)),
            Err(_) => notice.set(
                "The draft request was interrupted. Reload the workspace before trying again."
                    .to_owned(),
            ),
        }
        pending.set(false);
    });
}

fn submit_creation_intent(
    draft: RwSignal<Option<HeroCreationDraft>>,
    character: RwSignal<Option<HeroCharacter>>,
    pending: RwSignal<bool>,
    retry: RwSignal<Option<HeroCreationCommand>>,
    notice: RwSignal<String>,
    intent: Option<HeroCreationIntent>,
    campaign_refresh: CampaignRefresh,
) {
    let command = match (intent, retry.get_untracked()) {
        (None, Some(command)) => command,
        (None, None) => {
            notice.set("There is no interrupted hero choice to retry.".to_owned());
            return;
        }
        (Some(intent), _) => {
            let Some(current) = draft.get_untracked() else {
                notice.set("Reload the hero draft before choosing.".to_owned());
                return;
            };
            HeroCreationCommand {
                schema_version: HERO_COMMAND_SCHEMA_VERSION,
                draft_id: current.draft_id,
                expected_revision: current.revision,
                idempotency_key: uuid::Uuid::new_v4().to_string(),
                intent,
            }
        }
    };
    retry.set(Some(command.clone()));
    pending.set(true);
    notice.set("Validating and saving this exact choice…".to_owned());
    spawn_local(async move {
        match advance_hero_creation(command).await {
            Ok(HeroMutationResponse::Committed(result)) => {
                let result = *result;
                let completed = result.character.is_some();
                draft.set(Some(result.draft));
                if let Some(created) = result.character {
                    character.set(Some(created));
                }
                retry.set(None);
                notice.set(if completed {
                    "CharacterCreated committed atomically.".to_owned()
                } else {
                    "Choice saved. Continue with the next step.".to_owned()
                });
                campaign_refresh.reload();
            }
            Ok(HeroMutationResponse::Rejected(error)) => {
                let reload = matches!(error.code.as_str(), "hero_revision_conflict" | "hero_draft_expired" | "hero_not_found");
                notice.set(format_public_error(&error));
                if reload || !error.retryable {
                    retry.set(None);
                }
                if reload {
                    let loading = RwSignal::new(false);
                    load_workspace_into(draft, character, loading, notice);
                }
            }
            Err(_) => notice.set("The response was interrupted. Retry will reuse the identical choice and cannot commit twice.".to_owned()),
        }
        pending.set(false);
    });
}

#[derive(Clone, Copy)]
struct CampaignRefresh {
    view: RwSignal<Option<LocalCampaignViewDto>>,
    loading: RwSignal<bool>,
    notice: RwSignal<String>,
}

impl CampaignRefresh {
    fn reload(self) {
        crate::load_campaign_into(self.view, self.loading, self.notice);
    }
}

fn submit_level_up(
    character: RwSignal<Option<HeroCharacter>>,
    pending: RwSignal<bool>,
    retry: RwSignal<Option<LevelUpCommand>>,
    notice: RwSignal<String>,
    command: LevelUpCommand,
) {
    retry.set(Some(command.clone()));
    pending.set(true);
    notice.set("Validating and committing the level-up…".to_owned());
    spawn_local(async move {
        match advance_hero_level(command).await {
            Ok(HeroCharacterResponse::Committed(updated)) => {
                character.set(Some(*updated));
                retry.set(None);
                notice.set("Level 2 choices and derived sheet saved atomically.".to_owned());
            }
            Ok(HeroCharacterResponse::Rejected(error)) => {
                notice.set(format_public_error(&error));
                if !error.retryable {
                    retry.set(None);
                }
            }
            Err(_) => notice.set(
                "The response was interrupted. Retry reuses the same level-up command.".to_owned(),
            ),
        }
        pending.set(false);
    });
}

fn level_up_command(hero: &HeroCharacter) -> LevelUpCommand {
    let choice = match hero.choices.class.class() {
        manchester_dnd_core::hero::HeroClass::Fighter => LevelUpChoice::Fighter {
            hit_points: HitPointGrowthChoice::FixedAverage,
        },
        manchester_dnd_core::hero::HeroClass::Wizard => LevelUpChoice::Wizard {
            hit_points: HitPointGrowthChoice::FixedAverage,
            arcane_tradition: ArcaneTraditionId::Evocation,
        },
    };
    LevelUpCommand {
        schema_version: HERO_COMMAND_SCHEMA_VERSION,
        character_id: hero.character_id.clone(),
        expected_revision: hero.revision,
        idempotency_key: uuid::Uuid::new_v4().to_string(),
        choice,
    }
}

fn live_encounter_action_preview(class: manchester_dnd_core::hero::HeroClass, level: u8) -> String {
    match class {
        manchester_dnd_core::hero::HeroClass::Fighter if level >= 2 => {
            "Move, authored sluice interaction, weapon attacks, Second Wind, Action Surge, end turn"
                .to_owned()
        }
        manchester_dnd_core::hero::HeroClass::Fighter => {
            "Move, authored sluice interaction, weapon attacks, Second Wind, end turn".to_owned()
        }
        manchester_dnd_core::hero::HeroClass::Wizard => {
            "Move, authored sluice interaction, weapon attacks, Fire Bolt, Magic Missile, end turn"
                .to_owned()
        }
    }
}

fn fighter_assignment(agile: bool) -> StandardArrayAssignment {
    if agile {
        StandardArrayAssignment {
            strength: 13,
            dexterity: 15,
            constitution: 14,
            intelligence: 8,
            wisdom: 12,
            charisma: 10,
        }
    } else {
        StandardArrayAssignment {
            strength: 15,
            dexterity: 12,
            constitution: 14,
            intelligence: 8,
            wisdom: 13,
            charisma: 10,
        }
    }
}

fn wizard_assignment(resolute: bool) -> StandardArrayAssignment {
    if resolute {
        StandardArrayAssignment {
            strength: 8,
            dexterity: 13,
            constitution: 14,
            intelligence: 15,
            wisdom: 12,
            charisma: 10,
        }
    } else {
        StandardArrayAssignment {
            strength: 8,
            dexterity: 14,
            constitution: 13,
            intelligence: 15,
            wisdom: 12,
            charisma: 10,
        }
    }
}

fn background_selection(
    class: manchester_dnd_core::hero::HeroClass,
    background: BackgroundId,
) -> BackgroundSelection {
    let class_skills = match (class, background) {
        (manchester_dnd_core::hero::HeroClass::Fighter, BackgroundId::Soldier) => {
            vec![SkillId::Perception, SkillId::Survival]
        }
        (manchester_dnd_core::hero::HeroClass::Fighter, BackgroundId::Sage) => {
            vec![SkillId::Athletics, SkillId::Perception]
        }
        (manchester_dnd_core::hero::HeroClass::Wizard, BackgroundId::Soldier) => {
            vec![SkillId::Arcana, SkillId::Investigation]
        }
        (manchester_dnd_core::hero::HeroClass::Wizard, BackgroundId::Sage) => {
            vec![SkillId::Insight, SkillId::Investigation]
        }
    };
    BackgroundSelection {
        background,
        class_skills,
    }
}

fn background_detail(
    class: manchester_dnd_core::hero::HeroClass,
    background: BackgroundId,
) -> &'static str {
    match (class, background) {
        (manchester_dnd_core::hero::HeroClass::Fighter, BackgroundId::Soldier) => {
            "Athletics, Intimidation, Perception, Survival"
        }
        (manchester_dnd_core::hero::HeroClass::Fighter, BackgroundId::Sage) => {
            "Arcana, History, Athletics, Perception"
        }
        (manchester_dnd_core::hero::HeroClass::Wizard, BackgroundId::Soldier) => {
            "Athletics, Intimidation, Arcana, Investigation"
        }
        (manchester_dnd_core::hero::HeroClass::Wizard, BackgroundId::Sage) => {
            "Arcana, History, Insight, Investigation"
        }
    }
}

fn guard_equipment() -> EquipmentSelection {
    EquipmentSelection {
        carried: vec![
            EquipmentId::Longsword,
            EquipmentId::LightCrossbow,
            EquipmentId::Shield,
            EquipmentId::ChainMail,
            EquipmentId::ExplorersPack,
        ],
        simple_weapon: None,
        equipped_armor: Some(EquipmentId::ChainMail),
        shield_equipped: true,
    }
}

fn scout_equipment() -> EquipmentSelection {
    EquipmentSelection {
        carried: vec![
            EquipmentId::SimpleWeapons,
            EquipmentId::LightCrossbow,
            EquipmentId::LeatherArmor,
            EquipmentId::ExplorersPack,
        ],
        simple_weapon: Some(SimpleWeaponId::Spear),
        equipped_armor: Some(EquipmentId::LeatherArmor),
        shield_equipped: false,
    }
}

fn wizard_equipment(simple_weapon: SimpleWeaponId) -> EquipmentSelection {
    EquipmentSelection {
        carried: vec![
            EquipmentId::SimpleWeapons,
            EquipmentId::ScholarsPack,
            EquipmentId::Spellbook,
            EquipmentId::ArcaneFocus,
        ],
        simple_weapon: Some(simple_weapon),
        equipped_armor: None,
        shield_equipped: false,
    }
}

fn fixed_wizard_spells() -> WizardSpellSelection {
    WizardSpellSelection {
        cantrips: SpellId::CANTRIPS.to_vec(),
        spellbook: SpellId::LEVEL_ONE.to_vec(),
        prepared: SpellId::LEVEL_ONE.to_vec(),
    }
}

fn parsed_tone_limits(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .take(8)
        .map(ToOwned::to_owned)
        .collect()
}

fn step_label(step: CreationStep) -> String {
    let number = match step {
        CreationStep::CampaignTheme => 1,
        CreationStep::Concept => 2,
        CreationStep::Rules => 3,
        CreationStep::AbilityScores => 4,
        CreationStep::Background => 5,
        CreationStep::EquipmentAndSpells => 6,
        CreationStep::Presentation => 7,
        CreationStep::Review => 8,
        CreationStep::Commit => 9,
        CreationStep::Committed => 10,
    };
    format!("{number:02} / 10")
}

fn theme_label(theme: ThemeId) -> &'static str {
    match theme {
        ThemeId::RainboundBorough => "Rainbound Borough",
        ThemeId::EmberlineArchive => "Emberline Archive",
    }
}

fn theme_from_query(value: Option<&str>) -> &'static str {
    match value {
        Some("emberline-archive") => "Emberline Archive",
        Some("rainbound-borough") | None | Some(_) => "Rainbound Borough",
    }
}

fn class_selection_label(class: Option<&ClassSelection>) -> &'static str {
    match class {
        Some(ClassSelection::Fighter {
            fighting_style: FightingStyleId::Defense,
        }) => "Fighter (Defense)",
        Some(ClassSelection::Fighter {
            fighting_style: FightingStyleId::Dueling,
        }) => "Fighter (Dueling)",
        Some(ClassSelection::Wizard) => "Wizard",
        None => "—",
    }
}

fn format_optional<T: std::fmt::Debug>(value: Option<T>) -> String {
    value.map_or_else(|| "—".to_owned(), |value| format!("{value:?}"))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_offered_preset_is_domain_valid_when_composed() {
        for class in [
            ClassSelection::Fighter {
                fighting_style: FightingStyleId::Defense,
            },
            ClassSelection::Fighter {
                fighting_style: FightingStyleId::Dueling,
            },
            ClassSelection::Wizard,
        ] {
            let assignments = if matches!(class, ClassSelection::Wizard) {
                [wizard_assignment(false), wizard_assignment(true)]
            } else {
                [fighter_assignment(false), fighter_assignment(true)]
            };
            for assignment in assignments {
                assignment.validate().unwrap();
                for background in BackgroundId::ALL {
                    let selection = background_selection(class.class(), background);
                    for equipment in if matches!(class, ClassSelection::Wizard) {
                        vec![
                            wizard_equipment(SimpleWeaponId::Dagger),
                            wizard_equipment(SimpleWeaponId::Quarterstaff),
                        ]
                    } else {
                        vec![guard_equipment(), scout_equipment()]
                    } {
                        let choices = manchester_dnd_core::hero::HeroChoices {
                            pins: HeroPins::mvp(ThemeId::RainboundBorough),
                            concept: HeroConceptId::CanalGuardian,
                            ancestry: AncestryId::Human,
                            class: class.clone(),
                            ability_assignment: assignment.clone(),
                            background: selection.clone(),
                            equipment,
                            wizard_spells: matches!(class, ClassSelection::Wizard)
                                .then(fixed_wizard_spells),
                            presentation: HeroPresentation {
                                name: "Test Hero".to_owned(),
                                pronouns: "they/them".to_owned(),
                                appearance: "A test appearance".to_owned(),
                                ideal: "A test ideal".to_owned(),
                                bond: "A test bond".to_owned(),
                                flaw: "A test flaw".to_owned(),
                                tone_limits: vec!["No graphic gore".to_owned()],
                            },
                        };
                        choices.validate().unwrap();
                    }
                }
            }
        }
    }

    #[test]
    fn tone_limits_are_bounded_and_blank_entries_are_removed() {
        assert_eq!(
            parsed_tone_limits("No gore, , No spiders"),
            vec!["No gore", "No spiders"]
        );
        assert_eq!(parsed_tone_limits("a,b,c,d,e,f,g,h,i").len(), 8);
    }

    #[test]
    fn live_action_preview_names_only_integrated_encounter_mechanics() {
        assert_eq!(
            live_encounter_action_preview(manchester_dnd_core::hero::HeroClass::Wizard, 1,),
            "Move, authored sluice interaction, weapon attacks, Fire Bolt, Magic Missile, end turn"
        );
        let fighter_two =
            live_encounter_action_preview(manchester_dnd_core::hero::HeroClass::Fighter, 2);
        assert!(fighter_two.contains("Second Wind"));
        assert!(fighter_two.contains("Action Surge"));
        for deferred in ["Light", "Mage Hand", "Shield", "Sleep", "Arcane Recovery"] {
            assert!(!fighter_two.contains(deferred));
        }
    }
}
