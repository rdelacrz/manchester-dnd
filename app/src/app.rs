use leptos::prelude::*;
use leptos_meta::{MetaTags, Stylesheet, Title, provide_meta_context};
use leptos_router::{
    StaticSegment,
    components::{Route, Router, Routes},
};

use crate::components::info::{GuidePage, LegalPage, PrivacyAndSafetyPage};
use crate::components::layout::PublicLayout;
use crate::views::campaign_lobby::CampaignLobbyPage;
use crate::views::campaign_new::CampaignNewPage;
use crate::views::campaign_play::CampaignPlayPage;
use crate::views::campaigns::CampaignsPage;
use crate::views::character_campaign_stats::CharacterCampaignStatsPage;
use crate::views::character_detail::CharacterDetailPage;
use crate::views::character_new::CharacterNewPage;
use crate::views::characters::CharactersPage;
use crate::views::home::Home as LocalGame;
use crate::views::login::LoginPage;
use crate::views::signup::SignUpPage;
use leptos_router::ParamSegment;

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
        <a class="skip-link" href="#main-content">"Skip to main content"</a>
        <ErrorBoundary fallback=|_errors| view! { <AppError/> }>
            <Router>
                <Routes fallback=|| view! { <NotFound/> }.into_view()>
                    <Route path=StaticSegment("") view=HomePage/>
                    <Route path=StaticSegment("login") view=LoginPage/>
                    <Route path=StaticSegment("signup") view=SignUpPage/>
                    <Route path=StaticSegment("play") view=LocalGame/>
                    <Route path=StaticSegment("characters") view=CharactersPage/>
                    <Route path=StaticSegment("characters/new") view=CharacterNewPage/>
                    <Route path=(StaticSegment("characters"), ParamSegment("character_id")) view=CharacterDetailPage/>
                    <Route path=(StaticSegment("characters"), ParamSegment("character_id"), StaticSegment("campaigns"), ParamSegment("campaign_id"), StaticSegment("stats")) view=CharacterCampaignStatsPage/>
                    <Route path=StaticSegment("campaigns") view=CampaignsPage/>
                    <Route path=StaticSegment("campaigns/new") view=CampaignNewPage/>
                    <Route path=(StaticSegment("campaigns"), ParamSegment("id"), StaticSegment("lobby")) view=CampaignLobbyPage/>
                    <Route path=(StaticSegment("campaigns"), ParamSegment("id"), StaticSegment("play")) view=CampaignPlayPage/>
                    <Route path=StaticSegment("guide") view=GuidePage/>
                    <Route path=StaticSegment("privacy-and-safety") view=PrivacyAndSafetyPage/>
                    <Route path=StaticSegment("legal") view=LegalPage/>
                </Routes>
            </Router>
        </ErrorBoundary>
    }
}

/// Introduction-only home page. Contains marketing/introductory content only.
/// No campaign, character, encounter, or side navigation UI.
#[component]
fn HomePage() -> impl IntoView {
    view! {
        <Title text="Manchester Arcana · AI-guided tabletop adventure"/>
        <PublicLayout>
            <section class="hero-grid" data-testid="introduction-region">
                <div class="hero-copy">
                    <p class="eyebrow">"THE RAIN REMEMBERS"</p>
                    <h1>"Your city. Your stories. A realm remade."</h1>
                    <p class="lede">
                        "Build a hero, gather your party, and let an AI game master weave original fantasy with private, consented fragments of real life."
                    </p>
                    <div class="hero-actions">
                        <a class="primary-button" href="/signup">"Create an account"</a>
                        <a class="text-link" href="/login">"Log in →"</a>
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
                    <p>"Create an account, then build a hero, gather your party, explore the world, resolve encounters, and export your saved history."</p>
                </div>
                <ol>
                    <li><a href="/signup">"Create your account"</a></li>
                    <li><a href="/play">"Build a hero"</a></li>
                    <li>"Gather your party"</li>
                    <li>"Explore the world"</li>
                    <li>"Resolve encounters"</li>
                    <li>"Review and export history"</li>
                </ol>
                <p><a class="text-link" href="/guide">"Read safe setup, supported features, and known limits →"</a></p>
            </section>

            <section class="info-callout" aria-labelledby="play-now-heading">
                <h2 id="play-now-heading">"Ready to play?"</h2>
                <p>"The local single-player evaluation is available now. Create an account for hosted multiplayer features as they roll out."</p>
                <div class="hero-actions">
                    <a class="primary-button" href="/play">"Open the local game"</a>
                    <a class="text-link" href="/signup">"Sign up for multiplayer →"</a>
                </div>
            </section>
        </PublicLayout>
    }
}

#[component]
fn AppError() -> impl IntoView {
    view! {
        <PublicLayout>
            <section class="not-found" role="alert">
                <p class="eyebrow">"THE LANTERN FLICKERED"</p>
                <h1>"This view could not be shown safely."</h1>
                <p>"Your saved campaign was not changed. Reload the page or return home."</p>
                <a class="primary-button" href="/">"Return to the game"</a>
            </section>
        </PublicLayout>
    }
}

#[component]
fn NotFound() -> impl IntoView {
    view! {
        <PublicLayout>
            <section class="not-found">
                <p class="eyebrow">"LOST IN THE MISTS"</p>
                <h1>"That path is not on the map."</h1>
                <a class="primary-button" href="/">"Return to Manchester"</a>
            </section>
        </PublicLayout>
    }
}
