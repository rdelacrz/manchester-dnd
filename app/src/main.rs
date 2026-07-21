#![recursion_limit = "256"]

use axum::{
    Json, Router,
    body::Body,
    extract::{DefaultBodyLimit, Path as AxumPath, Request},
    http::{
        HeaderMap, HeaderValue, Method, StatusCode,
        header::{
            CACHE_CONTROL, CONTENT_ENCODING, CONTENT_LENGTH, CONTENT_SECURITY_POLICY, CONTENT_TYPE,
            HOST, ORIGIN, REFERRER_POLICY, X_CONTENT_TYPE_OPTIONS, X_FRAME_OPTIONS,
        },
        request::Parts,
    },
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use leptos::prelude::*;
use leptos_axum::{LeptosRoutes, generate_route_list};
use manchester_dnd_app::{App, auth_boundary, shell};
use manchester_dnd_core::is_valid_opaque_id;
use manchester_dnd_server::{
    AppConfig, ApplicationError, LOCAL_CAMPAIGN_SESSION_ID, RestoreCampaignExportCommand,
    ServerContext, repository::SceneImageVariant,
};
use tower_http::{compression::CompressionLayer, trace::TraceLayer};
use tracing::Instrument;

use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

const MAX_REQUEST_BODY_BYTES: usize = 64 * 1024;
const MAX_CAMPAIGN_RESTORE_BYTES: usize = 2 * 1024 * 1024;
const CAMPAIGN_RESTORE_PATH: &str = "/api/local/campaign/restore";
const CAMPAIGN_RESTORE_MEDIA_TYPE: &str =
    "application/vnd.manchester-arcana.campaign+json;version=1";
const CAMPAIGN_RESTORE_HEADER: &str = "x-manchester-arcana-restore";
const IDEMPOTENCY_KEY_HEADER: &str = "idempotency-key";
const CORRELATION_HEADER: &str = "x-correlation-id";
const SERVER_FN_ERROR_HEADER: &str = "serverfnerror";
const RATE_LIMIT_WINDOW: Duration = Duration::from_secs(60);
const MAX_MUTATIONS_PER_WINDOW: u32 = 300;
const MAX_RESTORES_PER_WINDOW: u32 = 6;
const MAX_IMAGE_READS_PER_WINDOW: u32 = 600;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Load dotenv-backed runtime settings before tracing and Leptos read the
    // process environment. Production environment variables retain priority.
    let app_config = AppConfig::load()?;

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "manchester_dnd=info,tower_http=info".into()),
        )
        .init();

    // Packaged releases have no source Cargo.toml and receive their complete
    // Leptos configuration through the environment. A direct `cargo run`
    // keeps the workspace-metadata fallback for development.
    let configuration = if std::env::var_os("LEPTOS_OUTPUT_NAME").is_some() {
        get_configuration(None)?
    } else {
        get_configuration(Some("Cargo.toml"))?
    };
    let address = configuration.leptos_options.site_addr;
    app_config.validate_bind_address(address)?;
    let leptos_options = configuration.leptos_options;
    let routes = generate_route_list(App);
    let server_context = ServerContext::from_config(app_config).await?;

    tracing::info!(
        content_pack_count = server_context.active_content.packs().len(),
        default_theme_pack_id = %server_context.active_content.default_theme().identity().id,
        private_inspiration_source_boundary = "database_minimized_only",
        "server dependencies initialized"
    );

    let readiness_context = server_context.clone();
    let restore_context = server_context.clone();
    let image_delivery_context = server_context.clone();
    let request_rate_limiter = RequestRateLimiter::new();
    let auth_boundary_state = auth_boundary::AuthenticationBoundaryState {
        context: server_context.clone(),
    };
    spawn_scene_image_worker(server_context.clone());
    let router = Router::new()
        .route("/health/live", get(|| async { StatusCode::NO_CONTENT }))
        .route(
            "/health/ready",
            get(move || {
                let context = readiness_context.clone();
                async move {
                    match context.health_check().await {
                        Ok(()) => StatusCode::NO_CONTENT,
                        Err(error) => {
                            tracing::warn!(
                                code = error.public_code(),
                                "readiness database check failed"
                            );
                            StatusCode::SERVICE_UNAVAILABLE
                        }
                    }
                }
            }),
        )
        .route(
            CAMPAIGN_RESTORE_PATH,
            post(move |headers: HeaderMap, body: Body| {
                let context = restore_context.clone();
                async move { restore_campaign_export(context, headers, body).await }
            }),
        )
        .route(
            "/api/local/images/{artifact_id}/{variant}",
            get(move |path| {
                let context = image_delivery_context.clone();
                async move { deliver_scene_image(context, path).await }
            }),
        )
        .leptos_routes_with_context(
            &leptos_options,
            routes,
            {
                let server_context = server_context.clone();
                move || {
                    provide_context(server_context.clone());
                    // The HTTP middleware owns the nonce so that the CSP header
                    // and every Leptos-generated inline script use exactly the
                    // same per-response value. Leptos creates a default nonce
                    // before this hook; providing ours replaces it.
                    if let Some(parts) = use_context::<Parts>()
                        && let Some(nonce) = parts.extensions.get::<leptos::nonce::Nonce>()
                    {
                        provide_context(nonce.clone());
                    }
                }
            },
            {
                let leptos_options = leptos_options.clone();
                move || shell(leptos_options.clone())
            },
        )
        .fallback(leptos_axum::file_and_error_handler(shell))
        .layer(DefaultBodyLimit::max(MAX_REQUEST_BODY_BYTES))
        .layer(CompressionLayer::new())
        .layer(TraceLayer::new_for_http())
        .layer(middleware::from_fn_with_state(
            request_rate_limiter,
            enforce_rate_limit,
        ))
        .layer(middleware::from_fn_with_state(
            auth_boundary_state,
            auth_boundary::resolve_request_principal,
        ))
        .layer(middleware::from_fn(enforce_public_boundary))
        .with_state(leptos_options);

    let listener = tokio::net::TcpListener::bind(&address).await?;
    tracing::info!(%address, "Manchester Arcana is listening");
    axum::serve(listener, router.into_make_service())
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    Ok(())
}

async fn shutdown_signal() {
    let interrupt = async {
        if let Err(error) = tokio::signal::ctrl_c().await {
            tracing::error!(%error, "failed to install interrupt handler");
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(error) => {
                tracing::error!(%error, "failed to install termination handler");
                std::future::pending::<()>().await;
            }
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = interrupt => {}
        () = terminate => {}
    }
    tracing::info!("shutdown signal received; draining HTTP requests");
}

#[derive(Clone)]
struct RequestRateLimiter {
    epoch: Instant,
    window: Arc<Mutex<RateWindow>>,
}

impl RequestRateLimiter {
    fn new() -> Self {
        Self {
            epoch: Instant::now(),
            window: Arc::new(Mutex::new(RateWindow::default())),
        }
    }

    fn check(&self, class: RateClass) -> RateDecision {
        let elapsed = self.epoch.elapsed();
        match self.window.lock() {
            Ok(mut window) => window.check(class, elapsed),
            Err(_) => RateDecision::Unavailable,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RateClass {
    Unmetered,
    Mutation,
    CampaignRestore,
    ImageRead,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RateDecision {
    Allowed,
    Limited,
    Unavailable,
}

#[derive(Debug, Default)]
struct RateWindow {
    index: u64,
    mutation_count: u32,
    restore_count: u32,
    image_read_count: u32,
}

impl RateWindow {
    fn check(&mut self, class: RateClass, elapsed: Duration) -> RateDecision {
        if class == RateClass::Unmetered {
            return RateDecision::Allowed;
        }
        let index = elapsed.as_secs() / RATE_LIMIT_WINDOW.as_secs();
        if index != self.index {
            self.index = index;
            self.mutation_count = 0;
            self.restore_count = 0;
            self.image_read_count = 0;
        }
        let (count, maximum) = match class {
            RateClass::Unmetered => return RateDecision::Allowed,
            RateClass::Mutation => (&mut self.mutation_count, MAX_MUTATIONS_PER_WINDOW),
            RateClass::CampaignRestore => (&mut self.restore_count, MAX_RESTORES_PER_WINDOW),
            RateClass::ImageRead => (&mut self.image_read_count, MAX_IMAGE_READS_PER_WINDOW),
        };
        if *count >= maximum {
            RateDecision::Limited
        } else {
            *count += 1;
            RateDecision::Allowed
        }
    }
}

async fn enforce_rate_limit(
    axum::extract::State(limiter): axum::extract::State<RequestRateLimiter>,
    request: Request,
    next: Next,
) -> Response {
    let class = rate_class(request.method(), request.uri().path());
    match limiter.check(class) {
        RateDecision::Allowed => next.run(request).await,
        RateDecision::Limited => {
            tracing::warn!(rate_class = ?class, code = "rate_limited", "local request rate limit reached");
            let mut response = public_boundary_error(StatusCode::TOO_MANY_REQUESTS, "rate_limited");
            response
                .headers_mut()
                .insert("retry-after", HeaderValue::from_static("60"));
            response
        }
        RateDecision::Unavailable => {
            tracing::error!(
                code = "rate_limit_unavailable",
                "local request rate limiter unavailable"
            );
            public_boundary_error(StatusCode::SERVICE_UNAVAILABLE, "rate_limit_unavailable")
        }
    }
}

fn rate_class(method: &Method, path: &str) -> RateClass {
    if path == CAMPAIGN_RESTORE_PATH {
        RateClass::CampaignRestore
    } else if method == Method::GET && path.starts_with("/api/local/images/") {
        RateClass::ImageRead
    } else if matches!(
        method,
        &Method::POST | &Method::PUT | &Method::PATCH | &Method::DELETE
    ) {
        RateClass::Mutation
    } else {
        RateClass::Unmetered
    }
}

fn spawn_scene_image_worker(context: ServerContext) {
    let worker_id = format!("scene-image-worker:{}", uuid::Uuid::new_v4());
    tokio::spawn(async move {
        let mut next_cleanup = tokio::time::Instant::now();
        loop {
            use manchester_dnd_server::SceneImageWorkerOutcome;

            if tokio::time::Instant::now() >= next_cleanup {
                if let Err(error) = context.scene_images.cleanup_expired(100).await {
                    tracing::warn!(
                        worker_id,
                        error_class = scene_image_error_class(&error),
                        "scene image retention cleanup failed"
                    );
                }
                next_cleanup = tokio::time::Instant::now() + std::time::Duration::from_secs(60);
            }

            let delay = match context.scene_images.process_next(&worker_id).await {
                Ok(SceneImageWorkerOutcome::Idle) => std::time::Duration::from_millis(250),
                Ok(SceneImageWorkerOutcome::CircuitOpen) => std::time::Duration::from_secs(1),
                Ok(
                    SceneImageWorkerOutcome::Succeeded
                    | SceneImageWorkerOutcome::RetryScheduled
                    | SceneImageWorkerOutcome::Failed
                    | SceneImageWorkerOutcome::LostLease,
                ) => std::time::Duration::from_millis(25),
                Err(error) => {
                    tracing::warn!(
                        worker_id,
                        error_class = scene_image_error_class(&error),
                        "scene image worker iteration failed"
                    );
                    std::time::Duration::from_secs(1)
                }
            };
            tokio::time::sleep(delay).await;
        }
    });
}

fn scene_image_error_class(error: &manchester_dnd_server::SceneImageError) -> &'static str {
    use manchester_dnd_server::SceneImageError;

    match error {
        SceneImageError::Disabled => "disabled",
        SceneImageError::InvalidCommand
        | SceneImageError::WrongCampaign
        | SceneImageError::RevisionConflict { .. }
        | SceneImageError::NoCommittedScene
        | SceneImageError::PolicyRejected
        | SceneImageError::BudgetExceeded
        | SceneImageError::ReplacementLimit
        | SceneImageError::NotFound => "request",
        SceneImageError::CircuitOpen | SceneImageError::Generation(_) => "provider",
        SceneImageError::BriefSerialization(_)
        | SceneImageError::Store(_)
        | SceneImageError::Repository(_)
        | SceneImageError::Storage(_)
        | SceneImageError::InvalidArtifact(_)
        | SceneImageError::Codec(_) => "internal",
    }
}

async fn deliver_scene_image(
    context: ServerContext,
    AxumPath((artifact_id, variant)): AxumPath<(String, String)>,
) -> Response {
    if !is_valid_opaque_id(&artifact_id) {
        return public_boundary_error(StatusCode::NOT_FOUND, "image_not_found");
    }
    let variant = match variant.as_str() {
        "web" => SceneImageVariant::Web,
        "thumbnail" => SceneImageVariant::Thumbnail,
        _ => return public_boundary_error(StatusCode::NOT_FOUND, "image_not_found"),
    };
    match context
        .scene_images
        .deliver(LOCAL_CAMPAIGN_SESSION_ID, &artifact_id, variant)
        .await
    {
        Ok(Some(image)) => {
            let mut response = Body::from(image.bytes).into_response();
            response.headers_mut().insert(
                CONTENT_TYPE,
                HeaderValue::from_str(&image.media_type)
                    .expect("validated image media types are header-safe"),
            );
            response.headers_mut().insert(
                CACHE_CONTROL,
                HeaderValue::from_static("private, max-age=300, no-transform"),
            );
            response
        }
        Ok(None) => public_boundary_error(StatusCode::NOT_FOUND, "image_not_found"),
        Err(error) => {
            tracing::warn!(
                error_class = scene_image_error_class(&error),
                "authorized scene image delivery failed"
            );
            public_boundary_error(StatusCode::SERVICE_UNAVAILABLE, "image_unavailable")
        }
    }
}

async fn enforce_public_boundary(mut request: Request, next: Next) -> Response {
    let correlation_id = uuid::Uuid::new_v4().to_string();
    let correlation_value = HeaderValue::from_str(&correlation_id)
        .expect("UUID correlation IDs are valid header values");
    let nonce = leptos::nonce::Nonce::new();
    request
        .headers_mut()
        .insert(CORRELATION_HEADER, correlation_value.clone());
    request.extensions_mut().insert(nonce.clone());

    let method = request.method().clone();
    let path = request.uri().path().to_owned();
    let span = tracing::info_span!(
        "http_request",
        correlation_id,
        method = %method,
        path = %path,
    );

    let mut response = async move {
        if !request.headers().get(HOST).is_some_and(valid_local_host) {
            return public_boundary_error(StatusCode::MISDIRECTED_REQUEST, "invalid_request_host");
        }
        if public_share_path_is_reserved(&path) {
            return public_boundary_error(StatusCode::NOT_FOUND, "public_sharing_unavailable");
        }
        if request_body_too_large(request.headers(), &path) {
            return public_boundary_error(StatusCode::PAYLOAD_TOO_LARGE, "request_too_large");
        }
        if method_has_body(request.method())
            && !supported_content_type_for_path(request.headers(), &path)
        {
            return public_boundary_error(
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "unsupported_content_type",
            );
        }
        if path == CAMPAIGN_RESTORE_PATH
            && let Err((status, code)) = validate_restore_headers(request.headers())
        {
            return public_boundary_error(status, code);
        }
        next.run(request).await
    }
    .instrument(span)
    .await;

    normalize_server_function_error(&mut response);
    add_private_response_headers(response.headers_mut(), &nonce);
    response
        .headers_mut()
        .insert(CORRELATION_HEADER, correlation_value);
    response
}

fn public_share_path_is_reserved(path: &str) -> bool {
    ["/share", "/api/share", "/public/campaign"]
        .into_iter()
        .any(|prefix| path == prefix || path.starts_with(&format!("{prefix}/")))
}

/// Leptos decodes server-function arguments before the typed function body is
/// entered. Its default transport response exposes decoder details as a 500.
/// In this application, typed functions return domain rejections as successful
/// envelopes, so a transport error means malformed input and is normalized to
/// one stable, non-sensitive public response.
fn normalize_server_function_error(response: &mut Response) {
    if response.status() == StatusCode::INTERNAL_SERVER_ERROR
        && response.headers().contains_key(SERVER_FN_ERROR_HEADER)
    {
        tracing::warn!(code = "invalid_server_input", "server input rejected");
        *response.status_mut() = StatusCode::BAD_REQUEST;
        response.headers_mut().remove(SERVER_FN_ERROR_HEADER);
        response.headers_mut().remove(CONTENT_ENCODING);
        response.headers_mut().remove(CONTENT_LENGTH);
        response.headers_mut().insert(
            CONTENT_TYPE,
            HeaderValue::from_static("application/json; charset=utf-8"),
        );
        *response.body_mut() = Body::from(r#"{"code":"invalid_server_input"}"#);
    }
}

fn valid_local_host(value: &HeaderValue) -> bool {
    let Ok(authority) = value
        .to_str()
        .ok()
        .and_then(|value| value.parse::<axum::http::uri::Authority>().ok())
        .ok_or(())
    else {
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

fn method_has_body(method: &Method) -> bool {
    matches!(method, &Method::POST | &Method::PUT | &Method::PATCH)
}

fn supported_content_type(headers: &HeaderMap) -> bool {
    headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .is_some_and(|media_type| {
            matches!(
                media_type.trim().to_ascii_lowercase().as_str(),
                "application/json" | "application/x-www-form-urlencoded" | "application/cbor"
            )
        })
}

fn supported_content_type_for_path(headers: &HeaderMap, path: &str) -> bool {
    if path == CAMPAIGN_RESTORE_PATH {
        headers
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value == CAMPAIGN_RESTORE_MEDIA_TYPE)
    } else {
        supported_content_type(headers)
    }
}

fn request_body_too_large(headers: &HeaderMap, path: &str) -> bool {
    let limit = if path == CAMPAIGN_RESTORE_PATH {
        MAX_CAMPAIGN_RESTORE_BYTES
    } else {
        MAX_REQUEST_BODY_BYTES
    };
    headers
        .get(CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<usize>().ok())
        .is_some_and(|length| length > limit)
}

fn validate_restore_headers(headers: &HeaderMap) -> Result<&str, (StatusCode, &'static str)> {
    if !supported_content_type_for_path(headers, CAMPAIGN_RESTORE_PATH) {
        return Err((
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "unsupported_content_type",
        ));
    }
    if headers
        .get(CAMPAIGN_RESTORE_HEADER)
        .and_then(|value| value.to_str().ok())
        != Some("1")
    {
        return Err((StatusCode::FORBIDDEN, "restore_confirmation_required"));
    }
    if !restore_request_is_same_origin(headers) {
        return Err((StatusCode::FORBIDDEN, "invalid_request_origin"));
    }
    let idempotency_key = headers
        .get(IDEMPOTENCY_KEY_HEADER)
        .and_then(|value| value.to_str().ok())
        .filter(|value| is_valid_opaque_id(value))
        .ok_or((StatusCode::BAD_REQUEST, "invalid_idempotency_key"))?;
    Ok(idempotency_key)
}

fn restore_request_is_same_origin(headers: &HeaderMap) -> bool {
    let Some(host) = headers.get(HOST).and_then(|value| value.to_str().ok()) else {
        return false;
    };
    let Some(origin) = headers.get(ORIGIN).and_then(|value| value.to_str().ok()) else {
        return false;
    };
    let Ok(origin) = origin.parse::<axum::http::Uri>() else {
        return false;
    };
    origin.scheme_str() == Some("http")
        && origin.path() == "/"
        && origin.query().is_none()
        && origin
            .authority()
            .is_some_and(|authority| authority.as_str().eq_ignore_ascii_case(host))
}

async fn restore_campaign_export(
    context: ServerContext,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let idempotency_key = match validate_restore_headers(&headers) {
        Ok(value) => value.to_owned(),
        Err((status, code)) => return public_boundary_error(status, code),
    };
    let canonical_export_json = match read_campaign_restore_body(body).await {
        Ok(value) => value,
        Err((status, code)) => return public_boundary_error(status, code),
    };
    let command = RestoreCampaignExportCommand {
        schema_version: 1,
        idempotency_key,
        canonical_export_json,
    };
    match context
        .application
        .restore_local_campaign_export(command)
        .await
    {
        Ok(outcome) => (StatusCode::CREATED, Json(outcome)).into_response(),
        Err(error) => {
            let status = restore_application_error_status(&error);
            tracing::warn!(code = error.public_code(), "campaign restore rejected");
            public_boundary_error(status, error.public_code())
        }
    }
}

async fn read_campaign_restore_body(body: Body) -> Result<String, (StatusCode, &'static str)> {
    let body = axum::body::to_bytes(body, MAX_CAMPAIGN_RESTORE_BYTES)
        .await
        .map_err(|_| (StatusCode::PAYLOAD_TOO_LARGE, "request_too_large"))?;
    String::from_utf8(body.to_vec())
        .map_err(|_| (StatusCode::BAD_REQUEST, "invalid_campaign_export"))
}

fn restore_application_error_status(error: &ApplicationError) -> StatusCode {
    match error {
        ApplicationError::HostedAccessDenied => StatusCode::FORBIDDEN,
        ApplicationError::LifecycleRevisionConflict { .. }
        | ApplicationError::IdempotencyConflict => StatusCode::CONFLICT,
        ApplicationError::InvalidCampaignExport
        | ApplicationError::InvalidCampaignLifecycle
        | ApplicationError::WrongCampaign => StatusCode::BAD_REQUEST,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

fn public_boundary_error(status: StatusCode, code: &'static str) -> Response {
    (
        status,
        [(CONTENT_TYPE, "application/json; charset=utf-8")],
        format!(r#"{{"code":"{code}"}}"#),
    )
        .into_response()
}

fn add_private_response_headers(headers: &mut HeaderMap, nonce: &leptos::nonce::Nonce) {
    // A hostile website must not be able to frame the loopback UI and trick a
    // player into issuing a same-origin command through clickjacking.
    if !headers.contains_key(CONTENT_SECURITY_POLICY) {
        let policy = format!(
            "default-src 'self'; base-uri 'none'; object-src 'none'; frame-ancestors 'none'; form-action 'self'; script-src 'nonce-{nonce}' 'strict-dynamic' 'wasm-unsafe-eval'; style-src 'self'; img-src 'self' data:; connect-src 'self' ws://127.0.0.1:* ws://localhost:*"
        );
        headers.insert(
            CONTENT_SECURITY_POLICY,
            HeaderValue::from_str(&policy).expect("Leptos nonces are valid CSP header values"),
        );
    }
    headers.insert(X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    headers.insert(X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    headers.insert(REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    headers.insert(
        CACHE_CONTROL,
        HeaderValue::from_static("no-store, max-age=0"),
    );
    headers.insert(
        "permissions-policy",
        HeaderValue::from_static("camera=(), microphone=(), geolocation=(), payment=()"),
    );
}

#[cfg(test)]
mod tests {
    use std::convert::Infallible;

    use axum::body::Bytes;
    use futures_util::stream;

    use super::*;

    fn valid_restore_headers() -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(HOST, HeaderValue::from_static("127.0.0.1:6789"));
        headers.insert(ORIGIN, HeaderValue::from_static("http://127.0.0.1:6789"));
        headers.insert(
            CONTENT_TYPE,
            HeaderValue::from_static(CAMPAIGN_RESTORE_MEDIA_TYPE),
        );
        headers.insert(CAMPAIGN_RESTORE_HEADER, HeaderValue::from_static("1"));
        headers.insert(
            IDEMPOTENCY_KEY_HEADER,
            HeaderValue::from_static("restore-request-1"),
        );
        headers
    }

    #[test]
    fn browser_responses_are_private_and_hardened() {
        let mut headers = HeaderMap::new();
        let nonce = leptos::nonce::Nonce::new();

        add_private_response_headers(&mut headers, &nonce);

        assert!(
            headers
                .get(CONTENT_SECURITY_POLICY)
                .unwrap()
                .to_str()
                .unwrap()
                .contains("frame-ancestors 'none'")
        );
        assert!(
            headers
                .get(CONTENT_SECURITY_POLICY)
                .unwrap()
                .to_str()
                .unwrap()
                .contains(&format!("'nonce-{nonce}'"))
        );
        assert_eq!(
            headers.get(X_FRAME_OPTIONS),
            Some(&HeaderValue::from_static("DENY"))
        );
        assert_eq!(
            headers.get(CACHE_CONTROL),
            Some(&HeaderValue::from_static("no-store, max-age=0"))
        );
    }

    #[test]
    fn host_content_type_and_size_boundaries_are_explicit() {
        assert!(valid_local_host(&HeaderValue::from_static(
            "127.0.0.1:6789"
        )));
        assert!(valid_local_host(&HeaderValue::from_static("[::1]:6789")));
        assert!(!valid_local_host(&HeaderValue::from_static(
            "malicious.example"
        )));

        let mut headers = HeaderMap::new();
        headers.insert(
            CONTENT_TYPE,
            HeaderValue::from_static("application/json; charset=utf-8"),
        );
        assert!(supported_content_type(&headers));
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("text/plain"));
        assert!(!supported_content_type(&headers));

        headers.insert(
            CONTENT_LENGTH,
            HeaderValue::from_str(&(MAX_REQUEST_BODY_BYTES + 1).to_string()).unwrap(),
        );
        assert!(request_body_too_large(&headers, "/api/ordinary"));
        assert!(!request_body_too_large(&headers, CAMPAIGN_RESTORE_PATH));

        headers.insert(
            CONTENT_LENGTH,
            HeaderValue::from_str(&(MAX_CAMPAIGN_RESTORE_BYTES + 1).to_string()).unwrap(),
        );
        assert!(request_body_too_large(&headers, CAMPAIGN_RESTORE_PATH));

        for path in [
            "/share",
            "/share/campaign-1",
            "/api/share/campaign-1",
            "/public/campaign/campaign-1",
        ] {
            assert!(public_share_path_is_reserved(path));
        }
        assert!(!public_share_path_is_reserved("/"));
        assert!(!public_share_path_is_reserved("/public/style.css"));
    }

    #[test]
    fn local_rate_limits_are_bounded_independent_and_reset_each_window() {
        assert_eq!(rate_class(&Method::GET, "/"), RateClass::Unmetered);
        assert_eq!(
            rate_class(&Method::POST, "/api/server-function"),
            RateClass::Mutation
        );
        assert_eq!(
            rate_class(&Method::POST, CAMPAIGN_RESTORE_PATH),
            RateClass::CampaignRestore
        );
        assert_eq!(
            rate_class(&Method::GET, "/api/local/images/artifact/web"),
            RateClass::ImageRead
        );

        let mut window = RateWindow::default();
        for _ in 0..MAX_RESTORES_PER_WINDOW {
            assert_eq!(
                window.check(RateClass::CampaignRestore, Duration::ZERO),
                RateDecision::Allowed
            );
        }
        assert_eq!(
            window.check(RateClass::CampaignRestore, Duration::ZERO),
            RateDecision::Limited
        );
        assert_eq!(
            window.check(RateClass::Mutation, Duration::ZERO),
            RateDecision::Allowed
        );
        assert_eq!(
            window.check(RateClass::CampaignRestore, RATE_LIMIT_WINDOW),
            RateDecision::Allowed
        );
    }

    #[test]
    fn campaign_restore_requires_exact_media_type_confirmation_and_origin() {
        let headers = valid_restore_headers();
        assert_eq!(validate_restore_headers(&headers), Ok("restore-request-1"));

        let mut forged_origin = headers.clone();
        forged_origin.insert(
            ORIGIN,
            HeaderValue::from_static("https://malicious.example"),
        );
        assert_eq!(
            validate_restore_headers(&forged_origin),
            Err((StatusCode::FORBIDDEN, "invalid_request_origin"))
        );

        let mut wrong_scheme = headers.clone();
        wrong_scheme.insert(ORIGIN, HeaderValue::from_static("https://127.0.0.1:6789"));
        assert_eq!(
            validate_restore_headers(&wrong_scheme),
            Err((StatusCode::FORBIDDEN, "invalid_request_origin"))
        );

        let mut missing_confirmation = headers.clone();
        missing_confirmation.remove(CAMPAIGN_RESTORE_HEADER);
        assert_eq!(
            validate_restore_headers(&missing_confirmation),
            Err((StatusCode::FORBIDDEN, "restore_confirmation_required"))
        );

        let mut loose_content_type = headers;
        loose_content_type.insert(
            CONTENT_TYPE,
            HeaderValue::from_static("application/vnd.manchester-arcana.campaign+json; version=1"),
        );
        assert_eq!(
            validate_restore_headers(&loose_content_type),
            Err((
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "unsupported_content_type"
            ))
        );
    }

    #[tokio::test]
    async fn campaign_restore_actual_chunked_body_is_capped_at_two_mibibytes() {
        let chunks = stream::iter([
            Ok::<Bytes, Infallible>(Bytes::from(vec![b'a'; MAX_CAMPAIGN_RESTORE_BYTES / 2])),
            Ok::<Bytes, Infallible>(Bytes::from(vec![b'b'; MAX_CAMPAIGN_RESTORE_BYTES / 2 + 1])),
        ]);

        assert_eq!(
            read_campaign_restore_body(Body::from_stream(chunks)).await,
            Err((StatusCode::PAYLOAD_TOO_LARGE, "request_too_large"))
        );

        assert_eq!(
            read_campaign_restore_body(Body::from("{\"schema_version\":1}"))
                .await
                .unwrap(),
            "{\"schema_version\":1}"
        );
    }

    #[tokio::test]
    async fn server_function_decoder_errors_are_stable_and_redacted() {
        let mut response = Response::builder()
            .status(StatusCode::INTERNAL_SERVER_ERROR)
            .header(SERVER_FN_ERROR_HEADER, "/api/private-function")
            .body(Body::from("Args|decoder implementation detail"))
            .unwrap();

        normalize_server_function_error(&mut response);

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert!(!response.headers().contains_key(SERVER_FN_ERROR_HEADER));
        assert_eq!(
            response.headers().get(CONTENT_TYPE),
            Some(&HeaderValue::from_static("application/json; charset=utf-8"))
        );
        let body = axum::body::to_bytes(response.into_body(), 1_024)
            .await
            .unwrap();
        assert_eq!(body.as_ref(), br#"{"code":"invalid_server_input"}"#);
    }
}
