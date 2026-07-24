//! Emits bounded MongoDB/queue/recovery health for an operator collector.

use std::process::ExitCode;

use manchester_dnd_server::{
    AppConfig, DatabaseOperationsSnapshot, MongoStore, repository::MongoRepository,
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
    let config = AppConfig::load().map_err(safe_error)?;
    let store = MongoStore::connect(&config.persistence.mongodb)
        .await
        .map_err(safe_error)?;
    let repository = MongoRepository::new(store);
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
