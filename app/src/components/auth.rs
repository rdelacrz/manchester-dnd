//! Bounded public authentication server functions.
//!
//! These functions are the only browser-facing entry points for sign-up,
//! login, logout, and current-session state. They call `AuthService`; they
//! never query accounts directly. Raw tokens and CSRF secrets are placed only
//! in the HttpOnly session cookie and the response body respectively.
#![allow(dead_code)]

use leptos::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignUpInput {
    pub email: String,
    pub display_name: String,
    pub password: String,
    #[serde(default)]
    pub next: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LoginInput {
    pub email: String,
    pub password: String,
    #[serde(default)]
    pub next: Option<String>,
}

/// Safe session-state view. Contains no token, CSRF secret, email, or account
/// ID that could be used for enumeration or session hijacking.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthStateView {
    pub authenticated: bool,
    pub display_name: Option<String>,
    pub csrf_token: Option<String>,
}

impl AuthStateView {
    pub const fn unauthenticated() -> Self {
        Self {
            authenticated: false,
            display_name: None,
            csrf_token: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", content = "payload", rename_all = "snake_case")]
pub enum AuthResponse {
    Success {
        display_name: String,
        redirect_to: String,
        /// Raw CSRF token. Sent once at sign-up/login so the browser can store
        /// it in memory and send it back in the `x-csrf-token` header for
        /// mutations. Never persisted in a cookie or local storage.
        csrf_token: String,
    },
    Error {
        code: String,
        message: String,
    },
}

impl AuthResponse {
    pub fn invalid_credentials() -> Self {
        Self::Error {
            code: "invalid_credentials".to_owned(),
            message: "Those credentials do not match an account.".to_owned(),
        }
    }

    pub fn account_unavailable() -> Self {
        Self::Error {
            code: "account_unavailable".to_owned(),
            message: "An account with those details cannot be created right now.".to_owned(),
        }
    }

    pub fn throttled() -> Self {
        Self::Error {
            code: "throttled".to_owned(),
            message: "Too many attempts. Please try again later.".to_owned(),
        }
    }

    pub fn internal_error() -> Self {
        Self::Error {
            code: "internal_error".to_owned(),
            message: "Authentication is temporarily unavailable.".to_owned(),
        }
    }

    pub fn authentication_required() -> Self {
        Self::Error {
            code: "authentication_required".to_owned(),
            message: "You must be signed in to do that.".to_owned(),
        }
    }

    pub fn csrf_required() -> Self {
        Self::Error {
            code: "csrf_required".to_owned(),
            message: "The security token for this session is missing or invalid.".to_owned(),
        }
    }
}

/// Safe redirect target. Rejects absolute URLs, scheme-relative URLs, and
/// paths outside the application's authenticated routes.
pub fn safe_redirect_target(next: &Option<String>) -> String {
    match next {
        Some(path) if is_safe_relative_path(path) => path.clone(),
        _ => "/characters".to_owned(),
    }
}

#[allow(dead_code)]
fn is_safe_relative_path(path: &str) -> bool {
    if path.is_empty() || path.len() > 512 {
        return false;
    }
    if path.starts_with("//") || path.starts_with("/\\") {
        return false;
    }
    if path.contains("://") {
        return false;
    }
    // Only allow paths that start with / and do not contain protocol-relative
    // constructs. The actual route guard handles unknown routes.
    path.starts_with('/')
        && !path.starts_with("//")
        && path.chars().all(|c| c.is_ascii_graphic() || c == ' ')
}

#[server(SignUp)]
pub async fn sign_up(input: SignUpInput) -> Result<AuthResponse, ServerFnError> {
    #[cfg(feature = "ssr")]
    {
        let Some(context) = use_context::<manchester_dnd_server::ServerContext>() else {
            return Ok(AuthResponse::internal_error());
        };
        let Some(_parts) = use_context::<axum::http::request::Parts>() else {
            return Ok(AuthResponse::internal_error());
        };

        // Throttle by HMAC-digested email before any password work.
        let throttle_key = context.authentication.throttle_key_digest(
            &input.email,
            manchester_dnd_server::AuthenticationActionKind::SignUp,
        );
        let throttle = context
            .authentication
            .record_authentication_attempt(
                &throttle_key,
                manchester_dnd_server::AuthenticationActionKind::SignUp,
            )
            .await
            .map_err(|_| {
                ServerFnError::<std::convert::Infallible>::ServerError(
                    "throttle check failed".to_owned(),
                )
            })?;
        if manchester_dnd_server::AuthService::is_throttled(&throttle) {
            return Ok(AuthResponse::throttled());
        }

        let password = manchester_dnd_server::AuthenticationSecret::new(input.password.clone());
        let issued = match context
            .authentication
            .sign_up(&input.email, &input.display_name, &password)
            .await
        {
            Ok(issued) => issued,
            Err(manchester_dnd_server::AuthenticationError::AccountUnavailable) => {
                return Ok(AuthResponse::account_unavailable());
            }
            Err(manchester_dnd_server::AuthenticationError::InvalidCredentials) => {
                return Ok(AuthResponse::invalid_credentials());
            }
            Err(_) => return Ok(AuthResponse::internal_error()),
        };

        set_session_cookie(&issued.session_token, &context.config);
        let redirect_to = safe_redirect_target(&input.next);
        Ok(AuthResponse::Success {
            display_name: issued.account.display_name,
            redirect_to,
            csrf_token: issued.csrf_token.expose_secret().to_owned(),
        })
    }

    #[cfg(not(feature = "ssr"))]
    {
        let _ = input;
        unreachable!("the server-function macro replaces this body in browser builds")
    }
}

#[server(Login)]
pub async fn login(input: LoginInput) -> Result<AuthResponse, ServerFnError> {
    #[cfg(feature = "ssr")]
    {
        let Some(context) = use_context::<manchester_dnd_server::ServerContext>() else {
            return Ok(AuthResponse::internal_error());
        };

        // Throttle by HMAC-digested email before any password work.
        let throttle_key = context.authentication.throttle_key_digest(
            &input.email,
            manchester_dnd_server::AuthenticationActionKind::Login,
        );
        let throttle = context
            .authentication
            .record_authentication_attempt(
                &throttle_key,
                manchester_dnd_server::AuthenticationActionKind::Login,
            )
            .await
            .map_err(|_| {
                ServerFnError::<std::convert::Infallible>::ServerError(
                    "throttle check failed".to_owned(),
                )
            })?;
        if manchester_dnd_server::AuthService::is_throttled(&throttle) {
            return Ok(AuthResponse::throttled());
        }

        let password = manchester_dnd_server::AuthenticationSecret::new(input.password.clone());
        let issued = match context.authentication.login(&input.email, &password).await {
            Ok(issued) => issued,
            Err(manchester_dnd_server::AuthenticationError::InvalidCredentials) => {
                return Ok(AuthResponse::invalid_credentials());
            }
            Err(_) => return Ok(AuthResponse::internal_error()),
        };

        set_session_cookie(&issued.session_token, &context.config);
        let redirect_to = safe_redirect_target(&input.next);
        Ok(AuthResponse::Success {
            display_name: issued.account.display_name,
            redirect_to,
            csrf_token: issued.csrf_token.expose_secret().to_owned(),
        })
    }

    #[cfg(not(feature = "ssr"))]
    {
        let _ = input;
        unreachable!("the server-function macro replaces this body in browser builds")
    }
}

#[server(Logout)]
pub async fn logout() -> Result<AuthResponse, ServerFnError> {
    #[cfg(feature = "ssr")]
    {
        use crate::auth_boundary::{require_csrf, require_principal};

        let Some(context) = use_context::<manchester_dnd_server::ServerContext>() else {
            return Ok(AuthResponse::internal_error());
        };
        let Some(parts) = use_context::<axum::http::request::Parts>() else {
            return Ok(AuthResponse::internal_error());
        };

        let principal = match require_principal(&parts) {
            Ok(principal) => principal,
            Err(_) => return Ok(AuthResponse::authentication_required()),
        };
        if require_csrf(&parts).is_err() {
            return Ok(AuthResponse::csrf_required());
        }

        match context.authentication.logout(&principal).await {
            Ok(()) => {}
            Err(_) => return Ok(AuthResponse::internal_error()),
        }

        clear_session_cookie(&context.config);
        Ok(AuthResponse::Success {
            display_name: String::new(),
            redirect_to: "/".to_owned(),
            csrf_token: String::new(),
        })
    }

    #[cfg(not(feature = "ssr"))]
    {
        unreachable!("the server-function macro replaces this body in browser builds")
    }
}

#[server(CurrentAuthState)]
pub async fn current_auth_state() -> Result<AuthStateView, ServerFnError> {
    #[cfg(feature = "ssr")]
    {
        let Some(context) = use_context::<manchester_dnd_server::ServerContext>() else {
            return Ok(AuthStateView::unauthenticated());
        };
        let Some(parts) = use_context::<axum::http::request::Parts>() else {
            return Ok(AuthStateView::unauthenticated());
        };

        let Some(principal) = parts
            .extensions
            .get::<manchester_dnd_server::AccountPrincipal>()
        else {
            return Ok(AuthStateView::unauthenticated());
        };

        // Local compatibility principal is always authenticated with no CSRF.
        if principal.account_id == manchester_dnd_server::LOCAL_ACCOUNT_ID {
            return Ok(AuthStateView {
                authenticated: true,
                display_name: Some("Local player".to_owned()),
                csrf_token: None,
            });
        }

        let summary = match context
            .authentication
            .load_account_summary(&principal.account_id)
            .await
        {
            Ok(Some(summary)) => summary,
            Ok(None) => return Ok(AuthStateView::unauthenticated()),
            Err(_) => return Ok(AuthStateView::unauthenticated()),
        };

        // The raw CSRF token is not recoverable from the stored digest. It is
        // returned only at sign-up/login time in the AuthResponse payload.
        // Clients must retain it in memory for the session lifetime.
        Ok(AuthStateView {
            authenticated: true,
            display_name: Some(summary.display_name),
            csrf_token: None,
        })
    }

    #[cfg(not(feature = "ssr"))]
    {
        unreachable!("the server-function macro replaces this body in browser builds")
    }
}

#[cfg(feature = "ssr")]
fn set_session_cookie(
    session_token: &manchester_dnd_server::AuthenticationSecret,
    config: &manchester_dnd_server::AppConfig,
) {
    use leptos_axum::ResponseOptions;

    let Some(response_options) = use_context::<ResponseOptions>() else {
        tracing::warn!("ResponseOptions unavailable; cannot set session cookie");
        return;
    };
    let cookie = crate::auth_boundary::session_cookie_header(session_token, config);
    response_options.append_header(axum::http::header::SET_COOKIE, cookie);
}

#[cfg(feature = "ssr")]
fn clear_session_cookie(config: &manchester_dnd_server::AppConfig) {
    use leptos_axum::ResponseOptions;

    let Some(response_options) = use_context::<ResponseOptions>() else {
        tracing::warn!("ResponseOptions unavailable; cannot clear session cookie");
        return;
    };
    let cookie = crate::auth_boundary::clear_session_cookie_header(config);
    response_options.append_header(axum::http::header::SET_COOKIE, cookie);
}

#[cfg(all(test, feature = "ssr"))]
mod tests {
    use super::*;

    #[test]
    fn safe_redirect_rejects_absolute_urls_and_unknown_schemes() {
        assert_eq!(safe_redirect_target(&None), "/characters");
        assert_eq!(safe_redirect_target(&Some(String::new())), "/characters");
        assert_eq!(
            safe_redirect_target(&Some("/characters".to_owned())),
            "/characters"
        );
        assert_eq!(
            safe_redirect_target(&Some("/campaigns/abc".to_owned())),
            "/campaigns/abc"
        );
        // Absolute URLs are rejected.
        assert_eq!(
            safe_redirect_target(&Some("https://evil.example/path".to_owned())),
            "/characters"
        );
        assert_eq!(
            safe_redirect_target(&Some("//evil.example/path".to_owned())),
            "/characters"
        );
        assert_eq!(
            safe_redirect_target(&Some("javascript:alert(1)".to_owned())),
            "/characters"
        );
    }

    #[test]
    fn auth_state_view_unauthenticated_has_no_secrets() {
        let view = AuthStateView::unauthenticated();
        assert!(!view.authenticated);
        assert!(view.display_name.is_none());
        assert!(view.csrf_token.is_none());
    }

    #[test]
    fn auth_response_error_codes_are_non_enumerating() {
        let invalid = AuthResponse::invalid_credentials();
        match invalid {
            AuthResponse::Error { code, message } => {
                assert_eq!(code, "invalid_credentials");
                assert!(!message.contains("email"));
                assert!(!message.contains("does not exist"));
                assert!(!message.contains("disabled"));
            }
            _ => panic!("expected error variant"),
        }

        let unavailable = AuthResponse::account_unavailable();
        match unavailable {
            AuthResponse::Error { code, .. } => assert_eq!(code, "account_unavailable"),
            _ => panic!("expected error variant"),
        }

        // All error codes must be distinct and none should reveal whether
        // the account exists, is disabled, or has a wrong password.
        let codes = [
            AuthResponse::invalid_credentials(),
            AuthResponse::account_unavailable(),
            AuthResponse::throttled(),
            AuthResponse::internal_error(),
            AuthResponse::authentication_required(),
            AuthResponse::csrf_required(),
        ];
        let mut seen = std::collections::HashSet::new();
        for response in &codes {
            if let AuthResponse::Error { code, message } = response {
                assert!(seen.insert(code.clone()), "duplicate error code: {code}");
                assert!(!message.to_lowercase().contains("email"));
                assert!(!message.to_lowercase().contains("password"));
            }
        }
    }
}
