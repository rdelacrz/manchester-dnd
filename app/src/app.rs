use leptos::prelude::*;
use leptos_meta::{MetaTags, Stylesheet, Title, provide_meta_context};
use leptos_router::{
    StaticSegment,
    components::{Route, Router, Routes},
};

use crate::components::info::{GuidePage, LegalPage, PrivacyAndSafetyPage};
use crate::views::home::Home;

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
                    <Route path=StaticSegment("") view=Home/>
                    <Route path=StaticSegment("guide") view=GuidePage/>
                    <Route path=StaticSegment("privacy-and-safety") view=PrivacyAndSafetyPage/>
                    <Route path=StaticSegment("legal") view=LegalPage/>
                </Routes>
            </Router>
        </ErrorBoundary>
    }
}

#[component]
fn AppError() -> impl IntoView {
    view! {
        <main class="not-found" role="alert">
            <p class="eyebrow">"THE LANTERN FLICKERED"</p>
            <h1>"This view could not be shown safely."</h1>
            <p>"Your saved campaign was not changed. Reload the page or return home."</p>
            <a class="primary-button" href="/">"Return to the game"</a>
        </main>
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
