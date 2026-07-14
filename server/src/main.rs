#![recursion_limit = "256"]

use axum::{
    Router,
    extract::Request,
    http::{
        HeaderMap, HeaderValue, StatusCode,
        header::{CONTENT_SECURITY_POLICY, X_FRAME_OPTIONS},
    },
    middleware::{self, Next},
    response::Response,
    routing::get,
};
use leptos::prelude::*;
use leptos_axum::{LeptosRoutes, generate_route_list};
use manchester_dnd_app::{App, shell};
use manchester_dnd_server::{AppConfig, ServerContext};
use tower_http::{compression::CompressionLayer, trace::TraceLayer};

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

    // Cargo Leptos provides these values as environment variables. Falling
    // back to workspace metadata also keeps a direct `cargo run` usable.
    let configuration = get_configuration(Some("Cargo.toml"))?;
    let address = configuration.leptos_options.site_addr;
    app_config.validate_bind_address(address)?;
    let leptos_options = configuration.leptos_options;
    let routes = generate_route_list(App);
    let server_context = ServerContext::from_config(app_config).await?;

    tracing::info!(
        event_prompt_count = server_context.event_prompts.len(),
        "server dependencies initialized"
    );

    let readiness_context = server_context.clone();
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
        .leptos_routes_with_context(
            &leptos_options,
            routes,
            {
                let server_context = server_context.clone();
                move || provide_context(server_context.clone())
            },
            {
                let leptos_options = leptos_options.clone();
                move || shell(leptos_options.clone())
            },
        )
        .fallback(leptos_axum::file_and_error_handler(shell))
        .layer(CompressionLayer::new())
        .layer(TraceLayer::new_for_http())
        .layer(middleware::from_fn(deny_framing))
        .with_state(leptos_options);

    let listener = tokio::net::TcpListener::bind(&address).await?;
    tracing::info!(%address, "Manchester Arcana is listening");
    axum::serve(listener, router.into_make_service()).await?;

    Ok(())
}

async fn deny_framing(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    add_anti_framing_headers(response.headers_mut());
    response
}

fn add_anti_framing_headers(headers: &mut HeaderMap) {
    // A hostile website must not be able to frame the loopback UI and trick a
    // player into issuing a same-origin command through clickjacking.
    headers.append(
        CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("frame-ancestors 'none'"),
    );
    headers.insert(X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn browser_responses_deny_framing() {
        let mut headers = HeaderMap::new();

        add_anti_framing_headers(&mut headers);

        assert_eq!(
            headers.get(CONTENT_SECURITY_POLICY),
            Some(&HeaderValue::from_static("frame-ancestors 'none'"))
        );
        assert_eq!(
            headers.get(X_FRAME_OPTIONS),
            Some(&HeaderValue::from_static("DENY"))
        );
    }
}
