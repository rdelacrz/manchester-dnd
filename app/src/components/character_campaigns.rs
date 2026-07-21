//! Character campaigns component — shows the list of campaign instances a
//! character appears in.
//!
//! This is a progressive-enhancement disclosure that lists authorized campaign
//! instances with links to their campaign stats pages. Full implementation
//! depends on campaign memberships (Task 12+). For now, it renders the
//! campaigns placeholder shown on the character detail page.

use leptos::prelude::*;

/// Placeholder component for the Campaigns (N) control.
/// Placeholder for the Campaigns (N) control. Full implementation with
/// campaign stats display comes in Task 12+ when campaign memberships exist.
#[component]
pub fn CharacterCampaigns(character_id: String) -> impl IntoView {
    view! {
        <div class="character-campaigns" data-testid="character-campaigns-component">
            <h2>"Campaigns (0)"</h2>
            <p>"This character has not been added to any campaigns yet."</p>
            {if !character_id.is_empty() {
                view! {
                    <p class="character-campaigns-hint">
                        <a href=format!("/characters/{}/campaigns/stats/local", character_id)>
                            "View local campaign stats →"
                        </a>
                    </p>
                }.into_any()
            } else {
                view! { <p></p> }.into_any()
            }}
        </div>
    }
}
