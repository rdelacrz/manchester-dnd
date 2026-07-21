//! Character detail page — shows a single character's library information.
//!
//! Displays identity and reusable creation choices only. No level, XP, HP,
//! or any campaign-derived runtime state is shown on this page. Campaign
//! instance stats are available via the campaign stats route (Task 12+).

use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_meta::Title;
use leptos_router::hooks::use_params;
use leptos_router::params::Params;

use crate::components::character_library::{
    CharacterDetailResponse, CharacterUpdateResponse, load_character, update_character_name,
};
use crate::components::protected_layout::ProtectedLayout;

#[component]
pub fn CharacterDetailPage() -> impl IntoView {
    let params = use_params::<CharacterDetailParams>();
    let character_id = move || {
        params
            .read()
            .as_ref()
            .map(|p| p.character_id.clone())
            .unwrap_or_default()
    };

    let character = Resource::new(character_id, move |id| async move {
        if id.is_empty() {
            return Ok(CharacterDetailResponse::NotFound);
        }
        load_character(id).await
    });

    view! {
        <Title text="Character · Manchester Arcana"/>
        <ProtectedLayout>
            <CharacterDetailContent character=character />
        </ProtectedLayout>
    }
}

#[derive(Params, PartialEq, Clone, Eq)]
pub struct CharacterDetailParams {
    pub character_id: String,
}

#[component]
fn CharacterDetailContent(
    character: Resource<Result<CharacterDetailResponse, ServerFnError>>,
) -> impl IntoView {
    view! {
        <section class="protected-page character-detail-page" aria-labelledby="character-detail-heading">
            <a class="back-link" href="/characters" data-testid="back-to-characters">"← Back to characters"</a>

            <Suspense fallback=move || {
                view! {
                    <div class="character-loading" role="status" aria-live="polite">
                        <p>"Loading character…"</p>
                    </div>
                }
            }>
                {move || match character.get() {
                    None => view! {
                        <div class="character-loading" role="status" aria-live="polite">
                            <p>"Loading character…"</p>
                        </div>
                    }.into_any(),
                    Some(Ok(CharacterDetailResponse::Found(detail))) => {
                        view! {
                            <div class="character-detail" data-testid="character-detail">
                                <h1 id="character-detail-heading">{detail.display_name.clone()}</h1>
                                <dl class="character-stats">
                                    <dt>"Theme"</dt><dd>{detail.theme_id.clone()}</dd>
                                    <dt>"Class"</dt><dd>{detail.class_name.clone()}</dd>
                                    <dt>"Ancestry"</dt><dd>{detail.ancestry_name.clone()}</dd>
                                    <dt>"Background"</dt><dd>{detail.background_name.clone()}</dd>
                                </dl>
                                <p class="character-revision">"Revision: " {detail.revision}</p>

                                <CharacterNameEditor
                                    character_id=detail.id.clone()
                                    current_name=detail.display_name.clone()
                                    revision=detail.revision
                                />

                                <crate::components::character_campaigns::CharacterCampaigns character_id=detail.id.clone() />
                            </div>
                        }.into_any()
                    }
                    Some(Ok(CharacterDetailResponse::NotFound)) => {
                        view! {
                            <div class="character-not-found" role="alert" data-testid="character-not-found">
                                <h1>"Character not found"</h1>
                                <p>"This character doesn't exist or you don't have access to it."</p>
                                <a class="primary-button" href="/characters">"Back to your characters"</a>
                            </div>
                        }.into_any()
                    }
                    Some(Ok(CharacterDetailResponse::Error { message, .. })) => {
                        view! {
                            <div class="character-error" role="alert" data-testid="character-error">
                                <h1>"Could not load character"</h1>
                                <p>{message.clone()}</p>
                            </div>
                        }.into_any()
                    }
                    Some(Err(_)) => {
                        view! {
                            <div class="character-error" role="alert" data-testid="character-error">
                                <h1>"Could not load character"</h1>
                                <p>"Character library is temporarily unavailable."</p>
                            </div>
                        }.into_any()
                    }
                }}
            </Suspense>
        </section>
    }
}

#[component]
fn CharacterNameEditor(character_id: String, current_name: String, revision: u64) -> impl IntoView {
    let character_id = StoredValue::new(character_id);
    let current_revision = RwSignal::new(revision);
    let display_name = RwSignal::new(current_name.clone());
    let editing = RwSignal::new(false);
    let saving = RwSignal::new(false);
    let error_msg = RwSignal::new(None::<String>);

    let on_save = move |_| {
        let cid = character_id.get_value();
        let rev = current_revision.get();
        let name = display_name.get();
        if name.trim().is_empty() {
            error_msg.set(Some("Name cannot be empty.".to_owned()));
            return;
        }
        saving.set(true);
        error_msg.set(None);
        spawn_local(async move {
            match update_character_name(cid, rev, name).await {
                Ok(CharacterUpdateResponse::Updated { revision }) => {
                    current_revision.set(revision);
                    editing.set(false);
                    saving.set(false);
                }
                Ok(CharacterUpdateResponse::NotFound) => {
                    error_msg.set(Some("Character not found.".to_owned()));
                    saving.set(false);
                }
                Ok(CharacterUpdateResponse::Error { message, .. }) => {
                    error_msg.set(Some(message));
                    saving.set(false);
                }
                Err(_) => {
                    error_msg.set(Some("Could not update name.".to_owned()));
                    saving.set(false);
                }
            }
        });
    };

    view! {
        <div class="character-name-editor">
            {move || if editing.get() {
                view! {
                    <form on:submit=move |ev| { ev.prevent_default(); } class="character-name-form">
                        <label for="character-name-input">"Display name"</label>
                        <input
                            id="character-name-input"
                            type="text"
                            prop:value=display_name
                            on:input=move |ev| display_name.set(event_target_value(&ev))
                            maxlength="200"
                            required
                        />
                        <button type="submit" disabled=saving on:click=on_save data-testid="save-name">
                            {move || if saving.get() { "Saving…" } else { "Save" }}
                        </button>
                        <button type="button" on:click=move |_| editing.set(false)>
                            "Cancel"
                        </button>
                    </form>
                }.into_any()
            } else {
                view! {
                    <button
                        type="button"
                        class="secondary-button"
                        on:click=move |_| editing.set(true)
                        data-testid="edit-name"
                    >
                        "Edit name"
                    </button>
                }.into_any()
            }}
            {move || error_msg.get().map(|msg| {
                view! { <span class="character-name-error" role="alert">{msg}</span> }
            })}
        </div>
    }
}
