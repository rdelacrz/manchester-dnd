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
                // No message may leak enumeration hints: the word "email" or
                // "password" would reveal which credential was wrong, and
                // "disabled"/"exist"/"wrong" would reveal account state. The
                // generic word "account" is permitted only in the
                // `account_unavailable` message, which describes creation
                // availability rather than revealing whether a specific
                // account exists.
                let lower = message.to_lowercase();
                assert!(!lower.contains("email"));
                assert!(!lower.contains("password"));
                assert!(!lower.contains("disabled"));
                assert!(!lower.contains("exist"));
                assert!(!lower.contains("wrong"));
            }
        }
    }

    /// Exhaustive redirect-target rejection matrix. Every payload here must
    /// fall back to the safe default ("/characters"); no absolute,
    /// scheme-relative, javascript:, empty, over-length, or non-ASCII value
    /// may ever reach the response body as a redirect target.
    #[test]
    fn safe_redirect_target_rejects_dangerous_and_malformed_inputs() {
        const SAFE_DEFAULT: &str = "/characters";

        // Empty input and missing redirect fall back to the safe default.
        assert_eq!(safe_redirect_target(&None), SAFE_DEFAULT);
        assert_eq!(safe_redirect_target(&Some(String::new())), SAFE_DEFAULT);

        // Absolute URLs with explicit schemes.
        for evil in [
            "https://evil.example/path",
            "http://evil.example/path",
            "ftp://evil.example/path",
            "https://evil.example.example/path",
        ] {
            assert_eq!(
                safe_redirect_target(&Some(evil.to_owned())),
                SAFE_DEFAULT,
                "absolute URL must be rejected: {evil}",
            );
        }

        // Scheme-relative URLs (//) — these resolve against the current origin
        // and must never be allowed.
        for evil in [
            "//evil.example/path",
            "//evil.example",
            "//localhost",
            "//  evil.example/path",
        ] {
            assert_eq!(
                safe_redirect_target(&Some(evil.to_owned())),
                SAFE_DEFAULT,
                "scheme-relative URL must be rejected: {evil}",
            );
        }

        // Backslash-prefixed scheme-relative constructs (/\ form).
        for evil in ["/\\evil.example/path", "/\\evil.example"] {
            assert_eq!(
                safe_redirect_target(&Some(evil.to_owned())),
                SAFE_DEFAULT,
                "backslash scheme-relative must be rejected: {evil}",
            );
        }

        // javascript: URIs and other dangerous schemes. Note these do not
        // start with `/`, so they fail the leading-slash check as well.
        for evil in [
            "javascript:alert(1)",
            "data:text/html,<script>alert(1)</script>",
            "vbscript:msgbox",
            "file:///etc/passwd",
        ] {
            assert_eq!(
                safe_redirect_target(&Some(evil.to_owned())),
                SAFE_DEFAULT,
                "dangerous scheme must be rejected: {evil}",
            );
        }

        // Paths over 512 characters must be rejected to prevent header
        // injection or DoS via oversized redirect values.
        let oversized = format!("/{}", "a".repeat(512));
        assert_eq!(oversized.len(), 513);
        assert_eq!(
            safe_redirect_target(&Some(oversized)),
            SAFE_DEFAULT,
            "path over 512 chars must be rejected",
        );

        // Paths with non-ASCII characters must be rejected (defence in depth
        // against unicode normalisation attacks and IDN homoglyphs).
        for evil in ["/café", "/日本語", "/foo\u{202e}bar", "/foo\0bar"] {
            assert_eq!(
                safe_redirect_target(&Some(evil.to_owned())),
                SAFE_DEFAULT,
                "non-ASCII or null-containing path must be rejected: {evil:?}",
            );
        }

        // Valid relative paths are preserved unchanged.
        assert_eq!(
            safe_redirect_target(&Some("/characters".to_owned())),
            "/characters",
        );
        assert_eq!(
            safe_redirect_target(&Some("/campaigns/abc-123".to_owned())),
            "/campaigns/abc-123",
        );
        // A single leading slash is the minimal accepted form.
        assert_eq!(safe_redirect_target(&Some("/".to_owned())), "/");
        // Spaces are ASCII-printable and currently permitted within paths.
        assert_eq!(
            safe_redirect_target(&Some("/path/with/space".to_owned())),
            "/path/with/space",
        );

        // The boundary value (exactly 512 chars) is accepted; 513 is not.
        let boundary_ok = format!("/{}", "a".repeat(511));
        assert_eq!(boundary_ok.len(), 512);
        assert_eq!(
            safe_redirect_target(&Some(boundary_ok.clone())),
            boundary_ok,
            "path of exactly 512 chars must be accepted",
        );
    }

    /// `is_safe_relative_path` is the inner gate that backs
    /// `safe_redirect_target`. It must reject backslash-based scheme-relative
    /// constructs, null bytes, and C0 control characters — these are the
    /// vectors used for header injection, path traversal, and smuggling
    /// attacks against redirect handlers.
    #[test]
    fn is_safe_relative_path_rejects_backslashes_null_bytes_and_controls() {
        // Backslash-based scheme-relative construct (/\ form) is always
        // rejected, even though it would otherwise pass the leading-slash
        // check. This blocks the historical browser quirk where `/\evil.com`
        // resolves to `//evil.com`.
        assert!(!is_safe_relative_path("/\\evil.example/path"));
        assert!(!is_safe_relative_path("/\\"));

        // Null bytes — never valid in a redirect target.
        assert!(!is_safe_relative_path("/foo\0bar"));
        assert!(!is_safe_relative_path("/\0"));

        // C0 control characters — must be rejected to prevent header
        // injection (CRLF) and other smuggling attacks.
        assert!(!is_safe_relative_path("/foo\nbar"));
        assert!(!is_safe_relative_path("/foo\rbar"));
        assert!(!is_safe_relative_path("/foo\tbar"));
        assert!(!is_safe_relative_path("/foo\u{7f}bar")); // DEL
        assert!(!is_safe_relative_path("/foo\u{1b}bar")); // ESC
        // A CRLF header-injection attempt must be blocked.
        assert!(!is_safe_relative_path("/foo\r\nSet-Cookie: bad=1"));

        // Scheme-relative (//) is rejected.
        assert!(!is_safe_relative_path("//evil.example"));
        assert!(!is_safe_relative_path("//localhost"));

        // Schemes embedded via `://`.
        assert!(!is_safe_relative_path("/foo://bar"));

        // Non-ASCII characters.
        assert!(!is_safe_relative_path("/café"));
        assert!(!is_safe_relative_path("/日本語"));

        // Empty and over-length.
        assert!(!is_safe_relative_path(""));
        assert!(!is_safe_relative_path(&format!("/{}", "a".repeat(512))));

        // Paths that do not start with `/` are never relative-to-root.
        assert!(!is_safe_relative_path("relative/path"));
        assert!(!is_safe_relative_path("javascript:alert(1)"));

        // Valid paths are accepted.
        assert!(is_safe_relative_path("/"));
        assert!(is_safe_relative_path("/characters"));
        assert!(is_safe_relative_path("/campaigns/abc-123"));
        assert!(is_safe_relative_path(&format!("/{}", "a".repeat(511)))); // boundary
        assert!(is_safe_relative_path("/path with/space"));
    }

    /// `AuthResponse::Success` is the only variant that carries a CSRF token
    /// and a redirect target. Its shape is a stable public contract: the CSRF
    /// token must be present (non-empty at sign-up/login), the redirect target
    /// must always be a safe relative path, and the display name must be
    /// present on success.
    #[test]
    fn auth_response_success_variant_carries_csrf_token_safe_redirect_and_display_name() {
        // A Success variant constructed with a non-empty CSRF token round-trips
        // through the tagged enum shape expected by the browser client.
        let success = AuthResponse::Success {
            display_name: "Tabletop Tanya".to_owned(),
            redirect_to: "/characters".to_owned(),
            csrf_token: "csrf-abc-123".to_owned(),
        };
        match &success {
            AuthResponse::Success {
                display_name,
                redirect_to,
                csrf_token,
            } => {
                assert!(!display_name.is_empty());
                assert!(!csrf_token.is_empty());
                // The redirect target must always be a safe relative path.
                assert!(
                    is_safe_relative_path(redirect_to),
                    "Success.redirect_to must be a safe relative path, got: {redirect_to}",
                );
                // It must start with a single leading slash.
                assert!(redirect_to.starts_with('/'));
                assert!(!redirect_to.starts_with("//"));
            }
            _ => panic!("expected Success variant"),
        }

        // Serialising the Success variant must yield the snake_case tag the
        // browser expects, and must not leak any field beyond display_name,
        // redirect_to, and csrf_token.
        let json = serde_json::to_string(&success).expect("must serialise");
        assert!(
            json.contains("\"status\":\"success\""),
            "expected success tag in JSON: {json}",
        );
        assert!(json.contains("\"display_name\":\"Tabletop Tanya\""));
        assert!(json.contains("\"redirect_to\":\"/characters\""));
        assert!(json.contains("\"csrf_token\":\"csrf-abc-123\""));
        // No envelope leak: no token/session/session_id/account_id keys.
        assert!(!json.contains("session_token"));
        assert!(!json.contains("session_id"));
        assert!(!json.contains("account_id"));

        // Round-trip back through deserialisation preserves all fields.
        let parsed: AuthResponse =
            serde_json::from_str(&json).expect("must deserialise back to AuthResponse");
        assert_eq!(&parsed, &success);

        // The Error variant must never carry a csrf_token field — that secret
        // is only ever emitted on success.
        let error = AuthResponse::invalid_credentials();
        let error_json = serde_json::to_string(&error).expect("must serialise error");
        assert!(
            error_json.contains("\"status\":\"error\""),
            "expected error tag in JSON: {error_json}",
        );
        assert!(
            !error_json.contains("csrf_token"),
            "csrf_token must never appear on Error responses: {error_json}",
        );
        assert!(
            !error_json.contains("redirect_to"),
            "redirect_to must never appear on Error responses: {error_json}",
        );

        // Logout emits a Success with an empty display name and csrf_token —
        // the contract is "no secrets on the wire for logout". The redirect
        // is the site root, which is still a safe relative path.
        let logout = AuthResponse::Success {
            display_name: String::new(),
            redirect_to: "/".to_owned(),
            csrf_token: String::new(),
        };
        match logout {
            AuthResponse::Success { redirect_to, .. } => {
                assert!(
                    is_safe_relative_path(&redirect_to),
                    "logout redirect_to must still be safe: {redirect_to}",
                );
            }
            _ => panic!("expected Success variant for logout"),
        }
    }

    /// `AuthStateView` is the polling response shape used by the browser to
    /// decide which navigation to render. The unauthenticated view must never
    /// leak a CSRF token, display name, or any other secret — and the
    /// authenticated shape must carry a display name but still never carry
    /// the raw CSRF secret (which is unrecoverable from its stored digest).
    #[test]
    fn auth_state_view_never_leaks_csrf_secret_or_display_name_when_unauthenticated() {
        let unauth = AuthStateView::unauthenticated();
        assert!(!unauth.authenticated);
        assert!(unauth.display_name.is_none());
        assert!(unauth.csrf_token.is_none());

        // The serialised form must carry exactly the three expected keys, and
        // csrf_token / display_name must be null — no secret may leak through
        // even as a non-null value. deny_unknown_fields enforces the shape.
        let json = serde_json::to_string(&unauth).expect("must serialise");
        assert!(
            json.contains("\"csrf_token\":null"),
            "unauthenticated csrf_token must be null: {json}",
        );
        assert!(
            json.contains("\"display_name\":null"),
            "unauthenticated display_name must be null: {json}",
        );
        assert!(json.contains("\"authenticated\":false"));
        // No session/identity fields may appear at all.
        assert!(!json.contains("session_token"));
        assert!(!json.contains("session_id"));
        assert!(!json.contains("account_id"));

        // An authenticated view (as built by current_auth_state for a real
        // session) carries a display name but never a raw CSRF token — the
        // raw token is sent exactly once, at sign-up/login time, in the
        // AuthResponse::Success payload. The view always has csrf_token=None.
        let authed = AuthStateView {
            authenticated: true,
            display_name: Some("Dungeon Master Dan".to_owned()),
            csrf_token: None,
        };
        let authed_json = serde_json::to_string(&authed).expect("must serialise");
        assert!(authed_json.contains("\"authenticated\":true"));
        assert!(authed_json.contains("\"display_name\":\"Dungeon Master Dan\""));
        assert!(
            authed_json.contains("\"csrf_token\":null"),
            "authenticated view must still have csrf_token=null (secret unrecoverable): {authed_json}",
        );
        // No session token leaks through the state view.
        assert!(!authed_json.contains("session_token"));
        assert!(!authed_json.contains("account_id"));
    }

    /// Every `AuthResponse::Error` constructor must produce an `Error`
    /// variant (never `Success`) and must serialise to the snake_case
    /// `"error"` status tag with no secret fields. This is the stable wire
    /// contract the browser depends on for showing the right UI.
    #[test]
    fn auth_response_error_variants_always_serialise_as_error_with_no_secrets() {
        for response in [
            AuthResponse::invalid_credentials(),
            AuthResponse::account_unavailable(),
            AuthResponse::throttled(),
            AuthResponse::internal_error(),
            AuthResponse::authentication_required(),
            AuthResponse::csrf_required(),
        ] {
            // Must be the Error variant.
            let (code, message) = match &response {
                AuthResponse::Error { code, message } => (code.clone(), message.clone()),
                _ => panic!("expected Error variant"),
            };
            assert!(!code.is_empty(), "error code must be non-empty");
            assert!(!message.is_empty(), "error message must be non-empty");

            // Wire shape: snake_case status tag, no secret fields.
            let json = serde_json::to_string(&response).expect("must serialise");
            assert!(
                json.contains("\"status\":\"error\""),
                "expected error tag: {json}",
            );
            assert!(
                !json.contains("csrf_token"),
                "csrf_token must never appear on Error responses: {json}",
            );
            assert!(
                !json.contains("redirect_to"),
                "redirect_to must never appear on Error responses: {json}",
            );
            assert!(
                !json.contains("session_token"),
                "session_token must never appear on Error responses: {json}",
            );
            assert!(
                !json.contains("display_name"),
                "display_name must never appear on Error responses: {json}",
            );

            // No error message may leak enumeration hints. "email"/"password"
            // would reveal which credential was wrong; "disabled"/"exist"/
            // "wrong" would reveal account state. The generic word "account"
            // appears only in account_unavailable, which describes creation
            // availability rather than revealing existence.
            let lower = message.to_lowercase();
            assert!(!lower.contains("email"));
            assert!(!lower.contains("password"));
            assert!(!lower.contains("disabled"));
            assert!(!lower.contains("exist"));
            assert!(!lower.contains("wrong"));
            assert!(!lower.contains("username"));
        }
    }
}
