use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_meta::{MetaTags, Stylesheet, Title, provide_meta_context};
use leptos_router::{
    StaticSegment,
    components::{Route, Router, Routes},
};
use manchester_dnd_core::{AbilityCheckResult, RollMode};

#[cfg(feature = "ssr")]
use manchester_dnd_core::{Ability, AbilityCheck, AbilityScores, Level, Proficiency, RollContext};

#[cfg(feature = "ssr")]
struct SystemDice;

#[cfg(feature = "ssr")]
impl manchester_dnd_core::DiceSource for SystemDice {
    fn roll(&mut self, sides: u16) -> u16 {
        use rand::Rng;

        rand::rng().random_range(1..=sides)
    }
}

/// A small end-to-end rules slice: the browser requests a roll, while the
/// server supplies entropy and the shared core crate resolves the check.
#[server]
pub async fn roll_demo_check(mode: RollMode) -> Result<AbilityCheckResult, ServerFnError> {
    #[cfg(feature = "ssr")]
    {
        let scores = AbilityScores::new(10, 10, 10, 10, 14, 10).map_err(ServerFnError::new)?;
        let level = Level::new(3).map_err(ServerFnError::new)?;
        let roll_context = match mode {
            RollMode::Normal => RollContext::normal(),
            RollMode::Advantage => RollContext::with_advantage(),
            RollMode::Disadvantage => RollContext::with_disadvantage(),
        };
        let check = AbilityCheck {
            ability: Ability::Wisdom,
            proficiency: Proficiency::Proficient,
            difficulty_class: 13,
            situational_modifier: 0,
            roll_context,
        };

        check
            .resolve(&scores, level, &mut SystemDice)
            .map_err(ServerFnError::new)
    }

    #[cfg(not(feature = "ssr"))]
    {
        let _ = mode;
        unreachable!("the server-function macro replaces this body in browser builds")
    }
}

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
    let roll_mode = RwSignal::new(RollMode::Normal);
    let roll_pending = RwSignal::new(false);
    let roll_summary = RwSignal::new(String::from("No check has been rolled yet."));

    let roll_check = move |_| {
        let mode = roll_mode.get();
        roll_pending.set(true);

        spawn_local(async move {
            let summary = match roll_demo_check(mode).await {
                Ok(result) => {
                    let dice = match result.roll.second {
                        Some(second) => format!(
                            "{} and {} → {}",
                            result.roll.first, second, result.roll.selected
                        ),
                        None => result.roll.selected.to_string(),
                    };
                    let outcome = if result.success { "success" } else { "setback" };
                    format!(
                        "Rolled {dice}; {} + {} ability + {} proficiency = {} vs DC {} — {outcome}.",
                        result.roll.selected,
                        result.ability_modifier,
                        result.proficiency_modifier,
                        result.total,
                        result.difficulty_class,
                    )
                }
                Err(error) => format!("The roll could not be completed: {error}"),
            };

            roll_summary.set(summary);
            roll_pending.set(false);
        });
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
                        <p class="roll-label">"Wisdom (Perception) · +4 · DC 13"</p>
                        <div class="roll-mode" aria-label="Roll mode">
                            <button
                                class:selected=move || roll_mode.get() == RollMode::Normal
                                on:click=move |_| roll_mode.set(RollMode::Normal)
                            >"Normal"</button>
                            <button
                                class:selected=move || roll_mode.get() == RollMode::Advantage
                                on:click=move |_| roll_mode.set(RollMode::Advantage)
                            >"Advantage"</button>
                            <button
                                class:selected=move || roll_mode.get() == RollMode::Disadvantage
                                on:click=move |_| roll_mode.set(RollMode::Disadvantage)
                            >"Disadvantage"</button>
                        </div>
                        <button
                            class="roll-button"
                            disabled=move || roll_pending.get()
                            on:click=roll_check
                        >
                            {move || if roll_pending.get() { "Rolling…" } else { "Roll the d20" }}
                        </button>
                        <p class="roll-readout" aria-live="polite">{move || roll_summary.get()}</p>
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
