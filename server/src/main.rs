use axum::Router;
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

    let configuration = get_configuration(None)?;
    let address = configuration.leptos_options.site_addr;
    let leptos_options = configuration.leptos_options;
    let routes = generate_route_list(App);
    let server_context = ServerContext::from_config(app_config).await?;

    tracing::info!(
        event_prompt_count = server_context.event_prompts.len(),
        "server dependencies initialized"
    );

    let router = Router::new()
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
        .with_state(leptos_options);

    let listener = tokio::net::TcpListener::bind(&address).await?;
    tracing::info!(%address, "Manchester Arcana is listening");
    axum::serve(listener, router.into_make_service()).await?;

    Ok(())
}
