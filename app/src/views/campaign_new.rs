use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_meta::Title;
use leptos_router::hooks::use_query_map;

use crate::components::protected_layout::ProtectedLayout;
use crate::views::campaigns::library::{
    CampaignLibraryResponse, CreateCampaignInput, create_campaign,
};

#[component]
pub fn CampaignNewPage() -> impl IntoView {
    let title = RwSignal::new(String::new());
    let theme_id = RwSignal::new("emberline".to_owned());
    let error_message = RwSignal::new(None::<String>);
    let pending = RwSignal::new(false);

    // Capture `next` for post-creation redirect, defaulting to /campaigns.
    let redirect_to = use_query_map()
        .get()
        .get("next")
        .filter(|p| !p.is_empty() && p.starts_with('/') && !p.starts_with("//"))
        .map(|s| s.to_owned())
        .unwrap_or_else(|| "/campaigns".to_owned());

    let submit = StoredValue::new(move || {
        let title_val = title.get();
        let theme_val = theme_id.get();
        if title_val.trim().is_empty() {
            error_message.set(Some("Enter a campaign title.".to_owned()));
            return;
        }
        pending.set(true);
        error_message.set(None);
        spawn_local(async move {
            match create_campaign(CreateCampaignInput {
                title: title_val,
                theme_id: theme_val,
            })
            .await
            {
                Ok(CampaignLibraryResponse::Ready { owned, .. }) => {
                    if let Some(c) = owned.first() {
                        let _ = window()
                            .location()
                            .set_href(&format!("/campaigns/{}/lobby", c.campaign_id));
                    } else {
                        let _ = window().location().set_href(&redirect_to);
                    }
                }
                Ok(CampaignLibraryResponse::Error { message, .. }) => {
                    error_message.set(Some(message));
                    pending.set(false);
                }
                Ok(CampaignLibraryResponse::AuthenticationRequired) => {
                    let _ = window()
                        .location()
                        .set_href(&format!("/login?next={redirect_to}"));
                }
                Err(_) => {
                    error_message.set(Some(
                        "Campaign creation is temporarily unavailable.".to_owned(),
                    ));
                    pending.set(false);
                }
            }
        });
    });

    view! {
        <Title text="Create campaign · Manchester Arcana"/>
        <ProtectedLayout>
            <section class="protected-page campaign-new-page" aria-labelledby="campaign-new-heading">
                <a class="back-link" href="/campaigns" data-testid="back-to-campaigns">
                    "← Back to campaigns"
                </a>
                <p class="eyebrow">"NEW CAMPAIGN"</p>
                <h1 id="campaign-new-heading">"Create a campaign"</h1>

                <form
                    class="auth-form"
                    on:submit=move |ev| {
                        ev.prevent_default();
                        submit.read_value();
                    }
                    novalidate
                >
                    <div class="form-field">
                        <label for="campaign-title">"Campaign title"</label>
                        <input
                            id="campaign-title"
                            type="text"
                            maxlength="200"
                            required
                            bind:value=title
                            aria-describedby="campaign-title-hint"
                        />
                        <p id="campaign-title-hint" class="form-hint">
                            "1–200 characters."
                        </p>
                    </div>

                    <div class="form-field">
                        <label for="campaign-theme">"Theme"</label>
                        <select id="campaign-theme" bind:value=theme_id>
                            <option value="emberline">"Emberline"</option>
                            <option value="rainbound">"Rainbound"</option>
                        </select>
                    </div>

                    {move || {
                        error_message.get().map(|msg| {
                            view! {
                                <p class="auth-error" role="alert" aria-live="assertive">
                                    {msg}
                                </p>
                            }
                        })
                    }}

                    <button
                        type="submit"
                        class="primary-button auth-submit"
                        disabled=pending.get()
                        aria-busy=pending.get()
                    >
                        {move || if pending.get() { "Creating…" } else { "Create campaign" }}
                    </button>
                </form>
            </section>
        </ProtectedLayout>
    }
}

#[cfg(all(test, feature = "ssr"))]
mod tests {
    #[test]
    fn campaign_new_module_compiles() {
        // ProtectedLayout uses Resource::new which requires a runtime executor.
        // We verify compilation here.
    }
}
