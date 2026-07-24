use std::sync::Arc;

use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_meta::Title;
use leptos_router::hooks::use_query_map;

use crate::components::layout::PublicLayout;
use crate::views::login::auth::{
    AuthResponse, BeginSignupInput, CompleteSignupInput, SignupChallengeResponse, begin_signup,
    complete_signup,
};

#[component]
pub fn SignUpPage() -> impl IntoView {
    let access_token = RwSignal::new(String::new());
    let signup_csrf = RwSignal::new(None::<String>);
    let email = RwSignal::new(String::new());
    let username = RwSignal::new(String::new());
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
        .map(|path| path.to_owned());

    let redeem_access = move || {
        let access_token_value = access_token.get();
        if access_token_value.trim().is_empty() {
            error_message.set(Some(
                "Enter the access code supplied by an administrator.".to_owned(),
            ));
            return;
        }
        pending.set(true);
        error_message.set(None);
        spawn_local(async move {
            match begin_signup(BeginSignupInput {
                access_token: access_token_value,
            })
            .await
            {
                Ok(SignupChallengeResponse::Ready { csrf_token }) => {
                    access_token.set(String::new());
                    signup_csrf.set(Some(csrf_token));
                    pending.set(false);
                }
                Ok(SignupChallengeResponse::Error { message, .. }) => {
                    error_message.set(Some(message));
                    pending.set(false);
                }
                Err(_) => {
                    error_message.set(Some("Sign-up is temporarily unavailable.".to_owned()));
                    pending.set(false);
                }
            }
        });
    };

    let create_account = move || {
        let Some(signup_csrf_token) = signup_csrf.get() else {
            error_message.set(Some("Validate your access code first.".to_owned()));
            return;
        };
        let email_value = email.get();
        let username_value = username.get();
        let password_value = password.get();
        if email_value.trim().is_empty()
            || username_value.trim().is_empty()
            || password_value.is_empty()
        {
            error_message.set(Some("Fill in all fields to create an account.".to_owned()));
            return;
        }
        pending.set(true);
        error_message.set(None);
        let next = redirect_to.clone();
        spawn_local(async move {
            match complete_signup(CompleteSignupInput {
                signup_csrf_token,
                email: email_value,
                username: username_value,
                password: password_value,
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
                            if let Some(window) = web_sys::window() {
                                if let Ok(Some(storage)) = window.session_storage() {
                                    let _ = storage.set_item("csrf", &csrf_token);
                                }
                            }
                        }
                    }
                    if window().location().set_href(&redirect_to).is_err() {
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
    let redeem_access = Arc::new(redeem_access);
    let create_account = Arc::new(create_account);

    view! {
        <Title text="Sign up · Manchester Arcana"/>
        <PublicLayout>
            <section class="auth-page" aria-labelledby="signup-heading">
                <div class="auth-card">
                    <p class="eyebrow">"BEGIN YOUR JOURNEY"</p>
                    <h1 id="signup-heading">"Create an account"</h1>
                    <p class="auth-subtitle">
                        {move || if signup_csrf.get().is_some() {
                            "Choose your account details. Your email is encrypted before storage."
                        } else {
                            "Sign-up requires a one-use access code generated by an administrator."
                        }}
                    </p>

                    {move || {
                        let redeem_access = Arc::clone(&redeem_access);
                        let create_account = Arc::clone(&create_account);
                        if signup_csrf.get().is_none() {
                        view! {
                            <form
                                class="auth-form"
                                on:submit=move |event| {
                                    event.prevent_default();
                                    redeem_access();
                                }
                                novalidate
                            >
                                <div class="form-field">
                                    <label for="signup-access-token">"Access code"</label>
                                    <input
                                        id="signup-access-token"
                                        type="password"
                                        autocomplete="one-time-code"
                                        required
                                        bind:value=access_token
                                        aria-describedby="signup-access-token-hint"
                                    />
                                    <p id="signup-access-token-hint" class="form-hint">
                                        "Codes can be used once and expire automatically."
                                    </p>
                                </div>
                                <SignupError message=error_message/>
                                <button
                                    type="submit"
                                    class="primary-button auth-submit"
                                    disabled=pending.get()
                                    aria-busy=pending.get()
                                >
                                    {move || if pending.get() { "Checking code…" } else { "Continue" }}
                                </button>
                            </form>
                        }.into_any()
                    } else {
                        view! {
                            <form
                                class="auth-form"
                                on:submit=move |event| {
                                    event.prevent_default();
                                    create_account();
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
                                    />
                                </div>
                                <div class="form-field">
                                    <label for="signup-username">"Username"</label>
                                    <input
                                        id="signup-username"
                                        type="text"
                                        autocomplete="username"
                                        required
                                        bind:value=username
                                        aria-describedby="signup-username-hint"
                                    />
                                    <p id="signup-username-hint" class="form-hint">
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
                                <SignupError message=error_message/>
                                <button
                                    type="submit"
                                    class="primary-button auth-submit"
                                    disabled=pending.get()
                                    aria-busy=pending.get()
                                >
                                    {move || if pending.get() { "Creating account…" } else { "Create account" }}
                                </button>
                            </form>
                        }.into_any()
                        }
                    }}

                    <p class="auth-alt-link">
                        "Already have an account? "
                        <a href="/login">"Log in"</a>
                    </p>
                </div>
            </section>
        </PublicLayout>
    }
}

#[component]
fn SignupError(message: RwSignal<Option<String>>) -> impl IntoView {
    move || {
        message.get().map(|message| {
            view! {
                <p class="auth-error" role="alert" aria-live="assertive">
                    {message}
                </p>
            }
        })
    }
}

#[cfg(all(test, feature = "ssr"))]
mod tests {
    use super::SignUpPage;
    use leptos::prelude::*;
    use leptos_router::components::Router;
    use leptos_router::location::RequestUrl;

    #[test]
    fn signup_starts_with_the_one_use_access_code_step() {
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
        assert!(html.contains(r#"for="signup-access-token""#));
        assert!(html.contains(r#"autocomplete="one-time-code""#));
        assert!(html.contains("one-use access code"));
        assert!(html.contains("/login"));
        assert!(!html.contains("signup-email"));
    }
}
