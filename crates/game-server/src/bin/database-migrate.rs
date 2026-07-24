//! Applies the managed MongoDB schema bundle.

use std::process::ExitCode;

use manchester_dnd_server::{AppConfig, MongoStore, SchemaReconciler};
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
    let config = AppConfig::load().map_err(|error| error.to_string())?;
    let store = MongoStore::connect(&config.persistence.mongodb)
        .await
        .map_err(|error| error.to_string())?;
    SchemaReconciler::new(store)
        .apply()
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
