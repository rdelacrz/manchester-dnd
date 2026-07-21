//! Campaign instance stats page — shows runtime hero stats for a character
//! in a specific campaign.
//!
//! This route displays current level, XP/progression, HP, derived stats,
//! features, resources, conditions, equipment, spells, and revision from
//! the selected runtime hero. Full implementation depends on campaign
//! memberships (Task 12+). For now, it shows a placeholder.

use leptos::prelude::*;
use leptos_meta::Title;
use leptos_router::hooks::use_params;
use leptos_router::params::Params;

use crate::components::protected_layout::ProtectedLayout;

#[derive(Params, PartialEq, Clone, Eq)]
pub struct CampaignStatsParams {
    pub character_id: String,
    pub campaign_id: String,
}

#[component]
pub fn CharacterCampaignStatsPage() -> impl IntoView {
    let params = use_params::<CampaignStatsParams>();
    let character_id = move || {
        params
            .read()
            .as_ref()
            .map(|p| p.character_id.clone())
            .unwrap_or_default()
    };
    let campaign_id = move || {
        params
            .read()
            .as_ref()
            .map(|p| p.campaign_id.clone())
            .unwrap_or_default()
    };

    view! {
        <Title text="Campaign stats · Manchester Arcana"/>
        <ProtectedLayout>
            <section class="protected-page character-campaign-stats-page" aria-labelledby="campaign-stats-heading">
                <a class="back-link" href=format!("/characters/{}", character_id()) data-testid="back-to-character">
                    "← Back to character"
                </a>
                <p class="eyebrow">"CAMPAIGN INSTANCE"</p>
                <h1 id="campaign-stats-heading">"Campaign stats"</h1>
                <div class="campaign-stats-placeholder" data-testid="campaign-stats-placeholder">
                    <p>
                        "Campaign instance stats will be available here once campaign memberships "
                        "are implemented."
                    </p>
                    <dl class="campaign-stats-meta">
                        <dt>"Character"</dt><dd><code>{character_id()}</code></dd>
                        <dt>"Campaign"</dt><dd><code>{campaign_id()}</code></dd>
                    </dl>
                    <p class="campaign-stats-hint">
                        "This page will show current level, XP/progression, HP, derived stats, "
                        "features, resources, conditions, equipment, spells, and revision."
                    </p>
                </div>
            </section>
        </ProtectedLayout>
    }
}
