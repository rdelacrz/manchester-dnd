//! Emits bounded PostgreSQL/queue/recovery health for an operator collector.

use std::{env, process::ExitCode};

use manchester_dnd_server::{
    DatabaseOperationsSnapshot, DatabaseRuntimeConfig, repository::PostgresRepository,
};
use serde_json::json;

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(snapshot) => match serde_json::to_string_pretty(&snapshot) {
            Ok(body) => {
                println!("{body}");
                ExitCode::SUCCESS
            }
            Err(_) => fail("database operations snapshot serialization failed"),
        },
        Err(message) => fail(&message),
    }
}

async fn run() -> Result<DatabaseOperationsSnapshot, String> {
    let database_url = env::var("DATABASE_URL")
        .map_err(|_| "DATABASE_URL is required for database operations".to_owned())?;
    if database_url.trim().is_empty() {
        return Err("DATABASE_URL is required for database operations".to_owned());
    }
    let runtime = DatabaseRuntimeConfig {
        max_connections: 1,
        migrate_on_start: false,
        ..DatabaseRuntimeConfig::default()
    };
    let repository = PostgresRepository::connect(&database_url, runtime)
        .await
        .map_err(safe_error)?;
    repository
        .database_operations_snapshot()
        .await
        .map_err(safe_error)
}

fn safe_error(error: impl std::fmt::Display) -> String {
    error.to_string()
}

fn fail(message: &str) -> ExitCode {
    eprintln!(
        "{}",
        serde_json::to_string_pretty(&json!({ "ok": false, "error": message }))
            .unwrap_or_else(|_| "{\"ok\":false,\"error\":\"command failed\"}".to_owned())
    );
    ExitCode::FAILURE
}
