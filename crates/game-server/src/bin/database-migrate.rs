//! Runs embedded PostgreSQL migrations with the deployment migration role.

use std::{env, process::ExitCode};

use manchester_dnd_server::{DatabaseRuntimeConfig, repository::PostgresRepository};
use serde_json::json;

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => {
            println!("{}", json!({ "ok": true, "migrated": true }));
            ExitCode::SUCCESS
        }
        Err(message) => fail(&message),
    }
}

async fn run() -> Result<(), String> {
    let database_url = env::var("DATABASE_URL")
        .map_err(|_| "DATABASE_URL is required for database migration".to_owned())?;
    if database_url.trim().is_empty() {
        return Err("DATABASE_URL is required for database migration".to_owned());
    }
    let runtime = DatabaseRuntimeConfig {
        max_connections: 1,
        migrate_on_start: true,
        ..DatabaseRuntimeConfig::default()
    };
    PostgresRepository::connect(&database_url, runtime)
        .await
        .map(|_| ())
        .map_err(|error| error.to_string())
}

fn fail(message: &str) -> ExitCode {
    eprintln!(
        "{}",
        serde_json::to_string_pretty(&json!({ "ok": false, "error": message }))
            .unwrap_or_else(|_| "{\"ok\":false,\"error\":\"command failed\"}".to_owned())
    );
    ExitCode::FAILURE
}
