//! Trusted HTTP account-session and CSRF boundary.
//!
//! Route guards are presentation only. Browser-facing handlers and server
//! functions must call `require_principal` and, for mutations, `require_csrf`.

use axum::{
    extract::{Request, State},
    http::{
        HeaderMap, HeaderValue, Method, StatusCode,
        header::{COOKIE, HOST, ORIGIN, SET_COOKIE},
        request::Parts,
    },
    middleware::Next,
    response::{IntoResponse, Response},
};
use manchester_dnd_server::{
    AccessMode, AccountPrincipal, AppConfig, AuthenticatedSession, AuthenticationError,
    AuthenticationSecret, LOCAL_ACCOUNT_ID, ServerContext,
};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

pub const SESSION_COOKIE_NAME: &str = "manchester_arcana_session";
pub const SIGNUP_SESSION_COOKIE_NAME: &str = "manchester_arcana_signup";
pub const CSRF_HEADER_NAME: &str = "x-csrf-token";
const LOCAL_COMPATIBILITY_SESSION_ID: &str = "session:local-compatibility";

#[derive(Clone)]
pub struct AuthenticationBoundaryState {
    pub context: ServerContext,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionSecurity {
    csrf_digest: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoundaryError {
    AuthenticationRequired,
    CsrfRequired,
    ServiceUnavailable,
}

impl BoundaryError {
    pub const fn status(self) -> StatusCode {
        match self {
            Self::AuthenticationRequired => StatusCode::UNAUTHORIZED,
            Self::CsrfRequired => StatusCode::FORBIDDEN,
            Self::ServiceUnavailable => StatusCode::SERVICE_UNAVAILABLE,
        }
    }

    pub const fn code(self) -> &'static str {
        match self {
            Self::AuthenticationRequired => "authentication_required",
            Self::CsrfRequired => "csrf_required",
            Self::ServiceUnavailable => "authentication_unavailable",
        }
    }
}

impl IntoResponse for BoundaryError {
    fn into_response(self) -> Response {
        (
            self.status(),
            [("content-type", "application/json; charset=utf-8")],
            format!(r#"{{"code":"{}"}}"#, self.code()),
        )
            .into_response()
    }
}

/// Resolves the host-only opaque session cookie and attaches only trusted
/// server-derived principal state. Invalid, expired, and revoked tokens are
/// treated identically to a missing cookie. Repository failure fails closed.
pub async fn resolve_request_principal(
    State(state): State<AuthenticationBoundaryState>,
    mut request: Request,
    next: Next,
) -> Response {
    match resolve_session(&state.context, request.headers()).await {
        Ok(Some(session)) => {
            request.extensions_mut().insert(session.principal.clone());
            request.extensions_mut().insert(SessionSecurity {
                csrf_digest: session.csrf_digest,
            });
        }
        Ok(None) => {}
        Err(BoundaryError::ServiceUnavailable) => {
            return BoundaryError::ServiceUnavailable.into_response();
        }
        Err(error) => return error.into_response(),
    }
    next.run(request).await
}

async fn resolve_session(
    context: &ServerContext,
    headers: &HeaderMap,
) -> Result<Option<AuthenticatedSession>, BoundaryError> {
    let token = session_cookie(headers);
    if let Some(token) = token {
        return match context
            .authentication
            .authenticate(&AuthenticationSecret::new(token))
            .await
        {
            Ok(session) => Ok(Some(session)),
            Err(AuthenticationError::InvalidSession) => Ok(None),
            Err(_) => Err(BoundaryError::ServiceUnavailable),
        };
    }

    if context.config.access_mode == AccessMode::LocalSingleUser {
        return Ok(Some(AuthenticatedSession {
            principal: AccountPrincipal {
                account_id: LOCAL_ACCOUNT_ID.to_owned(),
                session_id: LOCAL_COMPATIBILITY_SESSION_ID.to_owned(),
            },
            // Local compatibility mutations continue to rely on loopback Host
            // and strict Origin checks until account routes replace them.
            csrf_digest: String::new(),
            idle_expires_at: String::new(),
            absolute_expires_at: String::new(),
        }));
    }
    Ok(None)
}

pub fn require_principal(parts: &Parts) -> Result<AccountPrincipal, BoundaryError> {
    parts
        .extensions
        .get::<AccountPrincipal>()
        .cloned()
        .ok_or(BoundaryError::AuthenticationRequired)
}

pub fn require_csrf(parts: &Parts) -> Result<(), BoundaryError> {
    let principal = require_principal(parts)?;
    if principal.account_id == LOCAL_ACCOUNT_ID
        && principal.session_id == LOCAL_COMPATIBILITY_SESSION_ID
    {
        return Ok(());
    }
    let expected = parts
        .extensions
        .get::<SessionSecurity>()
        .ok_or(BoundaryError::CsrfRequired)?;
    let supplied = parts
        .headers
        .get(CSRF_HEADER_NAME)
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.is_empty())
        .ok_or(BoundaryError::CsrfRequired)?;
    let supplied_digest = sha256_digest(supplied.as_bytes());
    if constant_time_equal(expected.csrf_digest.as_bytes(), supplied_digest.as_bytes()) {
        Ok(())
    } else {
        Err(BoundaryError::CsrfRequired)
    }
}

pub fn require_csrf_for_method(parts: &Parts) -> Result<(), BoundaryError> {
    if is_mutation_method(&parts.method) {
        require_csrf(parts)
    } else {
        require_principal(parts).map(|_| ())
    }
}

pub fn session_cookie_header(raw_token: &AuthenticationSecret, config: &AppConfig) -> HeaderValue {
    let secure = if config.authentication.cookie_secure {
        "; Secure"
    } else {
        ""
    };
    HeaderValue::from_str(&format!(
        "{SESSION_COOKIE_NAME}={}; Path=/; HttpOnly; SameSite=Lax{secure}",
        raw_token.expose_secret()
    ))
    .expect("URL-safe session tokens and static cookie attributes are header-safe")
}

pub fn clear_session_cookie_header(config: &AppConfig) -> HeaderValue {
    let secure = if config.authentication.cookie_secure {
        "; Secure"
    } else {
        ""
    };
    HeaderValue::from_str(&format!(
        "{SESSION_COOKIE_NAME}=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0{secure}"
    ))
    .expect("static cookie attributes are header-safe")
}

pub fn append_session_cookie(
    headers: &mut HeaderMap,
    raw_token: &AuthenticationSecret,
    config: &AppConfig,
) {
    headers.append(SET_COOKIE, session_cookie_header(raw_token, config));
}

pub fn request_host_allowed(config: &AppConfig, headers: &HeaderMap) -> bool {
    let Some(host) = headers.get(HOST).and_then(|value| value.to_str().ok()) else {
        return false;
    };
    match config.access_mode {
        AccessMode::LocalSingleUser => local_authority(host),
        AccessMode::Hosted => config
            .authentication
            .canonical_origin
            .as_ref()
            .and_then(|origin| origin.parse::<axum::http::Uri>().ok())
            .and_then(|uri| {
                let host = uri.host()?.to_owned();
                Some(match uri.port() {
                    Some(port) => format!("{host}:{port}"),
                    None => host,
                })
            })
            .is_some_and(|authority| authority.eq_ignore_ascii_case(host)),
    }
}

/// Mutations require an exact configured origin. Local development accepts
/// HTTP only for a loopback Host and requires Origin authority to match Host.
pub fn request_origin_allowed(config: &AppConfig, headers: &HeaderMap, method: &Method) -> bool {
    if !is_mutation_method(method) {
        return true;
    }
    let Some(origin) = headers.get(ORIGIN).and_then(|value| value.to_str().ok()) else {
        return false;
    };
    match config.access_mode {
        AccessMode::LocalSingleUser => {
            let Some(host) = headers.get(HOST).and_then(|value| value.to_str().ok()) else {
                return false;
            };
            let Ok(origin) = origin.parse::<axum::http::Uri>() else {
                return false;
            };
            origin.scheme_str() == Some("http")
                && origin.path() == "/"
                && origin.query().is_none()
                && local_authority(host)
                && origin
                    .authority()
                    .is_some_and(|authority| authority.as_str().eq_ignore_ascii_case(host))
        }
        AccessMode::Hosted => config
            .authentication
            .canonical_origin
            .as_ref()
            .is_some_and(|configured| configured.as_str().trim_end_matches('/') == origin),
    }
}

fn session_cookie(headers: &HeaderMap) -> Option<String> {
    opaque_cookie(headers, SESSION_COOKIE_NAME)
}

pub fn signup_session_cookie(headers: &HeaderMap) -> Option<String> {
    opaque_cookie(headers, SIGNUP_SESSION_COOKIE_NAME)
}

fn opaque_cookie(headers: &HeaderMap, expected_name: &str) -> Option<String> {
    let mut found = None;
    for header in headers.get_all(COOKIE) {
        let raw = header.to_str().ok()?;
        for pair in raw.split(';') {
            let (name, value) = pair.trim().split_once('=')?;
            if name == expected_name {
                if found.is_some()
                    || value.is_empty()
                    || value.len() > 128
                    || !value
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
                {
                    return None;
                }
                found = Some(value.to_owned());
            }
        }
    }
    found
}

fn local_authority(raw: &str) -> bool {
    let Ok(authority) = raw.parse::<axum::http::uri::Authority>() else {
        return false;
    };
    let host = authority
        .host()
        .trim_start_matches('[')
        .trim_end_matches(']');
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|address| address.is_loopback())
}

fn is_mutation_method(method: &Method) -> bool {
    matches!(
        method,
        &Method::POST | &Method::PUT | &Method::PATCH | &Method::DELETE
    )
}

fn sha256_digest(value: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(value))
}

fn constant_time_equal(left: &[u8], right: &[u8]) -> bool {
    left.len() == right.len() && bool::from(left.ct_eq(right))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use manchester_dnd_server::AuthenticationConfig;

    use super::*;

    fn test_config(access_mode: AccessMode) -> AppConfig {
        let mut config = AppConfig::load().unwrap();
        config.access_mode = access_mode;
        let email_encryption_key_id = config.authentication.email_encryption_key_id.clone();
        let email_encryption_key = config.authentication.email_encryption_key.clone();
        let email_lookup_hmac_key = config.authentication.email_lookup_hmac_key.clone();
        config.authentication = AuthenticationConfig {
            session_idle_lifetime: Duration::from_secs(60),
            session_absolute_lifetime: Duration::from_secs(600),
            max_active_sessions: 3,
            max_hash_concurrency: 2,
            throttle_window_seconds: 300,
            throttle_block_after_attempts: 5,
            throttle_block_seconds: 60,
            throttle_hmac_key: manchester_dnd_server::SecretString::new("test-throttle-key"),
            cookie_secure: access_mode == AccessMode::Hosted,
            canonical_origin: (access_mode == AccessMode::Hosted)
                .then(|| "https://game.example.test".parse().unwrap()),
            argon2_memory_kib: 19_456,
            argon2_iterations: 2,
            argon2_parallelism: 1,
            email_encryption_key_id,
            email_encryption_key,
            email_lookup_hmac_key,
        };
        config
    }

    #[test]
    fn cookie_flags_are_host_only_http_only_lax_and_mode_secure() {
        let token = AuthenticationSecret::new("abc_123-safe");
        let local = session_cookie_header(&token, &test_config(AccessMode::LocalSingleUser));
        let local = local.to_str().unwrap();
        assert!(local.contains("HttpOnly"));
        assert!(local.contains("SameSite=Lax"));
        assert!(local.contains("Path=/"));
        assert!(!local.contains("Domain="));
        assert!(!local.contains("Secure"));

        let hosted = session_cookie_header(&token, &test_config(AccessMode::Hosted));
        assert!(hosted.to_str().unwrap().contains("; Secure"));
        assert!(
            clear_session_cookie_header(&test_config(AccessMode::Hosted))
                .to_str()
                .unwrap()
                .contains("Max-Age=0")
        );
    }

    #[test]
    fn local_and_hosted_host_origin_policies_are_separate_and_exact() {
        let mut headers = HeaderMap::new();
        headers.insert(HOST, HeaderValue::from_static("127.0.0.1:6789"));
        headers.insert(ORIGIN, HeaderValue::from_static("http://127.0.0.1:6789"));
        let local = test_config(AccessMode::LocalSingleUser);
        assert!(request_host_allowed(&local, &headers));
        assert!(request_origin_allowed(&local, &headers, &Method::POST));

        let hosted = test_config(AccessMode::Hosted);
        assert!(!request_host_allowed(&hosted, &headers));
        headers.insert(HOST, HeaderValue::from_static("game.example.test"));
        headers.insert(
            ORIGIN,
            HeaderValue::from_static("https://game.example.test"),
        );
        assert!(request_host_allowed(&hosted, &headers));
        assert!(request_origin_allowed(&hosted, &headers, &Method::POST));
        headers.insert(
            ORIGIN,
            HeaderValue::from_static("https://evil.example.test"),
        );
        assert!(!request_origin_allowed(&hosted, &headers, &Method::POST));
    }

    #[test]
    fn csrf_comparison_and_cookie_parsing_fail_closed() {
        assert!(constant_time_equal(b"same", b"same"));
        assert!(!constant_time_equal(b"same", b"diff"));
        assert!(!constant_time_equal(b"short", b"longer"));

        let mut headers = HeaderMap::new();
        headers.insert(
            COOKIE,
            HeaderValue::from_static("other=1; manchester_arcana_session=abc_123"),
        );
        assert_eq!(session_cookie(&headers).as_deref(), Some("abc_123"));
        headers.append(
            COOKIE,
            HeaderValue::from_static("manchester_arcana_session=duplicate"),
        );
        assert_eq!(session_cookie(&headers), None);
    }

    #[test]
    fn principal_and_csrf_helpers_reject_missing_mismatched_and_accept_exact_tokens() {
        let token = "csrf-token-from-session";
        let request = Request::builder()
            .method(Method::POST)
            .header(CSRF_HEADER_NAME, token)
            .body(())
            .unwrap();
        let (mut parts, ()) = request.into_parts();
        assert_eq!(
            require_principal(&parts),
            Err(BoundaryError::AuthenticationRequired)
        );
        parts.extensions.insert(AccountPrincipal {
            account_id: "account:11111111-1111-4111-8111-111111111111".to_owned(),
            session_id: "session:22222222-2222-4222-8222-222222222222".to_owned(),
        });
        parts.extensions.insert(SessionSecurity {
            csrf_digest: sha256_digest(token.as_bytes()),
        });
        assert_eq!(require_csrf(&parts), Ok(()));

        parts
            .headers
            .insert(CSRF_HEADER_NAME, HeaderValue::from_static("wrong-token"));
        assert_eq!(require_csrf(&parts), Err(BoundaryError::CsrfRequired));
        parts.headers.remove(CSRF_HEADER_NAME);
        assert_eq!(require_csrf(&parts), Err(BoundaryError::CsrfRequired));
    }
}
