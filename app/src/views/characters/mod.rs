//! Characters library page — lists the authenticated account's characters.
//!
//! Shows empty, loading, error, and character-card states. No level, XP, HP,
//! or any campaign-derived runtime state is displayed on this page.

use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_meta::Title;

pub(crate) mod library;

use self::library::{
    CharacterDeleteResponse, CharacterListResponse, delete_character, list_characters,
};
use crate::components::protected_layout::ProtectedLayout;

#[component]
pub fn CharactersPage() -> impl IntoView {
    let characters = Resource::new(move || (), move |_| async { list_characters().await });

    view! {
        <Title text="Characters · Manchester Arcana"/>
        <ProtectedLayout>
            <CharactersContent characters=characters />
        </ProtectedLayout>
    }
}

#[component]
fn CharactersContent(
    characters: Resource<Result<CharacterListResponse, ServerFnError>>,
) -> impl IntoView {
    view! {
        <section class="protected-page characters-page" aria-labelledby="characters-heading">
            <div class="characters-header">
                <div>
                    <p class="eyebrow">"YOUR CAST"</p>
                    <h1 id="characters-heading">"Characters"</h1>
                </div>
                <a class="primary-button" href="/characters/new" data-testid="create-character-link">
                    "Create a character"
                </a>
            </div>

            <Suspense fallback=move || {
                view! {
                    <div class="characters-loading" role="status" aria-live="polite">
                        <p>"Loading your characters…"</p>
                    </div>
                }
            }>
                {move || match characters.get() {
                    None => view! {
                        <div class="characters-loading" role="status" aria-live="polite">
                            <p>"Loading your characters…"</p>
                        </div>
                    }.into_any(),
                    Some(Ok(CharacterListResponse::Success { characters })) => {
                        if characters.is_empty() {
                            view! {
                                <div class="characters-empty" data-testid="characters-empty">
                                    <p>"You don't have any characters yet."</p>
                                    <a class="primary-button" href="/characters/new">
                                        "Create your first character"
                                    </a>
                                </div>
                            }.into_any()
                        } else {
                            view! {
                                <ul class="character-list" data-testid="character-list">
                                    {characters.into_iter().map(|c| {
                                        view! {
                                            <li class="character-card" data-testid="character-card">
                                                <a class="character-card-link"
                                                   href=format!("/characters/{}", &c.id)
                                                   data-testid="character-card-link">
                                                    <span class="character-card-name">{c.display_name.clone()}</span>
                                                </a>
                                                <DeleteButton character_id=c.id.clone() revision=c.revision />
                                            </li>
                                        }
                                    }).collect::<Vec<_>>()}
                                </ul>
                            }.into_any()
                        }
                    }
                    Some(Ok(CharacterListResponse::Error { code, message })) => {
                        view! {
                            <div class="characters-error" role="alert" data-testid="characters-error">
                                <p>{message.clone()}</p>
                                <p class="error-code">"Error: " {code.clone()}</p>
                            </div>
                        }.into_any()
                    }
                    Some(Err(_)) => {
                        view! {
                            <div class="characters-error" role="alert" data-testid="characters-error">
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
fn DeleteButton(character_id: String, revision: u64) -> impl IntoView {
    let character_id = StoredValue::new(character_id);
    let _ = revision;
    let deleting = RwSignal::new(false);
    let error_msg = RwSignal::new(None::<String>);

    let on_delete = move |_| {
        let cid = character_id.get_value();
        deleting.set(true);
        error_msg.set(None);
        spawn_local(async move {
            match delete_character(cid.clone()).await {
                Ok(CharacterDeleteResponse::Deleted) => {
                    // Reload the page to refresh the list.
                    window().location().reload().ok();
                }
                Ok(CharacterDeleteResponse::NotFound) => {
                    error_msg.set(Some("Character not found.".to_owned()));
                    deleting.set(false);
                }
                Ok(CharacterDeleteResponse::Error { message, .. }) => {
                    error_msg.set(Some(message));
                    deleting.set(false);
                }
                Err(_) => {
                    error_msg.set(Some("Could not delete character.".to_owned()));
                    deleting.set(false);
                }
            }
        });
    };

    view! {
        <button
            class="character-delete-button"
            type="button"
            aria-label="Delete character"
            disabled=deleting
            on:click=on_delete
            data-testid="character-delete-button"
        >
            {move || if deleting.get() { "Deleting…" } else { "Delete" }}
        </button>
        {move || error_msg.get().map(|msg| {
            view! {
                <span class="character-delete-error" role="alert">{msg}</span>
            }
        })}
    }
}

#[cfg(all(test, feature = "ssr"))]
mod tests {
    #[test]
    fn characters_module_compiles() {
        // Placeholder — browser tests cover the UI behavior.
    }
}
