//! Character creation page — entry point for the character creation wizard.
//!
//! In the current phase, this page provides a simple creation form that
//! submits to a server function. The full multi-step wizard UI from the local
//! game will be migrated here in a subsequent task. For now, it provides a
//! link to the local game's hero creation wizard as a transitional measure.

use leptos::prelude::*;
use leptos_meta::Title;

use crate::components::protected_layout::ProtectedLayout;

#[component]
pub fn CharacterNewPage() -> impl IntoView {
    view! {
        <Title text="Create a character · Manchester Arcana"/>
        <ProtectedLayout>
            <section class="protected-page character-new-page" aria-labelledby="character-new-heading">
                <a class="back-link" href="/characters" data-testid="back-to-characters">
                    "← Back to characters"
                </a>
                <p class="eyebrow">"NEW HERO"</p>
                <h1 id="character-new-heading">"Create a character"</h1>
                <p>
                    "Character creation is being migrated to account-owned storage. "
                    "The full multi-step wizard will be available here soon."
                </p>
                <div class="character-new-options">
                    <a class="primary-button" href="/play" data-testid="use-local-creation">
                        "Use the local game's creation wizard"
                    </a>
                    <p class="character-new-hint">
                        "Characters created in the local game will be importable to your library "
                        "once the migration is complete."
                    </p>
                </div>
            </section>
        </ProtectedLayout>
    }
}
