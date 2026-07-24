use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_meta::Title;
use leptos_router::hooks::use_query_map;

use crate::components::auth::{AuthResponse, LoginInput, login};
use crate::components::layout::PublicLayout;

#[component]
pub fn LoginPage() -> impl IntoView {
    let identifier = RwSignal::new(String::new());
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
        let identifier_value = identifier.get();
        let password_val = password.get();
        if identifier_value.trim().is_empty() || password_val.is_empty() {
            error_message.set(Some(
                "Enter your username or email and password.".to_owned(),
            ));
            return;
        }
        pending.set(true);
        error_message.set(None);
        let next = redirect_to.clone();
        spawn_local(async move {
            match login(LoginInput {
                identifier: identifier_value,
                password: password_val,
                next,
            })
            .await
            {
                Ok(AuthResponse::Success {
                    display_name: _,
                    redirect_to,
                    csrf_token,
                }) => {
                    // Session storage avoids retaining the CSRF token after the
                    // browser session closes.
                    if !csrf_token.is_empty() {
                        #[cfg(target_arch = "wasm32")]
                        {
                            if let Some(win) = web_sys::window() {
                                if let Ok(Some(storage)) = win.session_storage() {
                                    let _ = storage.set_item("csrf", &csrf_token);
                                }
                            }
                        }
                    }
                    let navigated = window().location().set_href(&redirect_to);
                    if navigated.is_err() {
                        error_message.set(Some("Could not redirect after login.".to_owned()));
                        pending.set(false);
                    }
                }
                Ok(AuthResponse::Error { message, .. }) => {
                    error_message.set(Some(message));
                    pending.set(false);
                }
                Err(_) => {
                    error_message.set(Some(
                        "Authentication is temporarily unavailable.".to_owned(),
                    ));
                    pending.set(false);
                }
            }
        });
    };

    view! {
        <Title text="Log in · Manchester Arcana"/>
        <PublicLayout>
            <section class="auth-page" aria-labelledby="login-heading">
                <div class="auth-card">
                    <p class="eyebrow">"WELCOME BACK"</p>
                    <h1 id="login-heading">"Log in to your account"</h1>
                    <p class="auth-subtitle">
                        "Enter your username or email and password to return to your campaigns."
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
                            <label for="login-identifier">"Username or email"</label>
                            <input
                                id="login-identifier"
                                type="text"
                                autocomplete="username"
                                required
                                bind:value=identifier
                                aria-describedby="login-identifier-hint"
                            />
                            <p id="login-identifier-hint" class="form-hint">
                                "Either identifier works."
                            </p>
                        </div>

                        <div class="form-field">
                            <label for="login-password">"Password"</label>
                            <input
                                id="login-password"
                                type="password"
                                autocomplete="current-password"
                                required
                                bind:value=password
                                aria-describedby="login-password-hint"
                            />
                            <p id="login-password-hint" class="form-hint">
                                "Password managers are supported. Paste is welcome."
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
                            {move || if pending.get() { "Signing in…" } else { "Log in" }}
                        </button>
                    </form>

                    <p class="auth-alt-link">
                        "Don't have an account? "
                        <a href="/signup">"Sign up"</a>
                    </p>
                </div>
            </section>
        </PublicLayout>
    }
}

#[cfg(all(test, feature = "ssr"))]
mod tests {
    use super::LoginPage;
    use leptos::prelude::*;
    use leptos_router::components::Router;
    use leptos_router::location::RequestUrl;

    #[test]
    fn login_page_renders_form_with_accessible_labels_and_autocomplete() {
        let owner = Owner::new();
        let html = owner.with(|| {
            provide_context(RequestUrl::new("/login"));
            view! {
                <Router>
                    <LoginPage/>
                </Router>
            }
            .to_html()
        });
        assert!(html.contains(r#"id="login-heading""#));
        assert!(html.contains(r#"for="login-identifier""#));
        assert!(html.contains(r#"autocomplete="username""#));
        assert!(html.contains(r#"autocomplete="current-password""#));
        assert!(html.contains(r#"type="text""#));
        assert!(html.contains(r#"type="password""#));
        assert!(html.contains(r#"type="submit""#));
        assert!(html.contains("/signup"));
        assert!(!html.contains("csrf"));
    }
}
