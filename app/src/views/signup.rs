use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_meta::Title;
use leptos_router::hooks::use_query_map;

use crate::components::auth::{AuthResponse, SignUpInput, sign_up};
use crate::components::layout::PublicLayout;

#[component]
pub fn SignUpPage() -> impl IntoView {
    let email = RwSignal::new(String::new());
    let display_name = RwSignal::new(String::new());
    let password = RwSignal::new(String::new());
    let error_message = RwSignal::new(None::<String>);
    let pending = RwSignal::new(false);
    let redirect_to = use_query_map()
        .get()
        .get("next")
        .filter(|path| {
            !path.is_empty()
                && path.starts_with('/')
                && !path.starts_with("//")
                && !path.contains("://")
        })
        .map(|s| s.to_owned());

    let submit = move || {
        let email_val = email.get();
        let display_val = display_name.get();
        let password_val = password.get();
        if email_val.trim().is_empty() || display_val.trim().is_empty() || password_val.is_empty() {
            error_message.set(Some("Fill in all fields to create an account.".to_owned()));
            return;
        }
        pending.set(true);
        error_message.set(None);
        let next = redirect_to.clone();
        spawn_local(async move {
            match sign_up(SignUpInput {
                email: email_val,
                display_name: display_val,
                password: password_val,
                next,
            })
            .await
            {
                Ok(AuthResponse::Success {
                    redirect_to,
                    csrf_token,
                    ..
                }) => {
                    if !csrf_token.is_empty() {
                        #[cfg(target_arch = "wasm32")]
                        {
                            if let Some(win) = web_sys::window() {
                                if let Ok(Some(storage)) = win.local_storage() {
                                    let _ = storage.set_item("csrf", &csrf_token);
                                }
                            }
                        }
                    }
                    let navigated = window().location().set_href(&redirect_to);
                    if navigated.is_err() {
                        error_message.set(Some("Could not redirect after sign up.".to_owned()));
                        pending.set(false);
                    }
                }
                Ok(AuthResponse::Error { message, .. }) => {
                    error_message.set(Some(message));
                    pending.set(false);
                }
                Err(_) => {
                    error_message.set(Some(
                        "Account creation is temporarily unavailable.".to_owned(),
                    ));
                    pending.set(false);
                }
            }
        });
    };

    view! {
        <Title text="Sign up · Manchester Arcana"/>
        <PublicLayout>
            <section class="auth-page" aria-labelledby="signup-heading">
                <div class="auth-card">
                    <p class="eyebrow">"BEGIN YOUR JOURNEY"</p>
                    <h1 id="signup-heading">"Create an account"</h1>
                    <p class="auth-subtitle">
                        "Your email, display name, and a strong password are all you need."
                    </p>

                    <form
                        class="auth-form"
                        on:submit=move |ev| {
                            ev.prevent_default();
                            submit();
                        }
                        novalidate
                    >
                        <div class="form-field">
                            <label for="signup-email">"Email"</label>
                            <input
                                id="signup-email"
                                type="email"
                                autocomplete="email"
                                required
                                bind:value=email
                                aria-describedby="signup-email-hint"
                            />
                            <p id="signup-email-hint" class="form-hint">
                                "We never display your email to other players."
                            </p>
                        </div>

                        <div class="form-field">
                            <label for="signup-name">"Display name"</label>
                            <input
                                id="signup-name"
                                type="text"
                                autocomplete="name"
                                required
                                bind:value=display_name
                                aria-describedby="signup-name-hint"
                            />
                            <p id="signup-name-hint" class="form-hint">
                                "This is what other players will see."
                            </p>
                        </div>

                        <div class="form-field">
                            <label for="signup-password">"Password"</label>
                            <input
                                id="signup-password"
                                type="password"
                                autocomplete="new-password"
                                required
                                bind:value=password
                                aria-describedby="signup-password-hint"
                            />
                            <p id="signup-password-hint" class="form-hint">
                                "Use 15–128 characters. Passphrases are welcome."
                            </p>
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
                            {move || if pending.get() { "Creating account…" } else { "Sign up" }}
                        </button>
                    </form>

                    <p class="auth-alt-link">
                        "Already have an account? "
                        <a href="/login">"Log in"</a>
                    </p>
                </div>
            </section>
        </PublicLayout>
    }
}

#[cfg(all(test, feature = "ssr"))]
mod tests {
    use super::SignUpPage;
    use leptos::prelude::*;
    use leptos_router::components::Router;
    use leptos_router::location::RequestUrl;

    #[test]
    fn signup_page_renders_form_with_accessible_labels_and_autocomplete() {
        let owner = Owner::new();
        let html = owner.with(|| {
            provide_context(RequestUrl::new("/signup"));
            view! {
                <Router>
                    <SignUpPage/>
                </Router>
            }
            .to_html()
        });
        assert!(html.contains(r#"id="signup-heading""#));
        assert!(html.contains(r#"for="signup-email""#));
        assert!(html.contains(r#"autocomplete="email""#));
        assert!(html.contains(r#"autocomplete="new-password""#));
        assert!(html.contains(r#"autocomplete="name""#));
        assert!(html.contains("/login"));
    }
}
