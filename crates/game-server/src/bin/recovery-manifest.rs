//! Emits a body-free, deterministic recovery manifest for the fixed local owner.

use std::{env, path::Path, process::ExitCode};

use manchester_dnd_server::{
    AppConfig, CompleteRecoveryManifest, LOCAL_HERO_OWNER_KEY, MongoStore,
    repository::MongoRepository,
};
use serde_json::json;

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(manifest) => match serde_json::to_string_pretty(&manifest) {
            Ok(body) => {
                println!("{body}");
                ExitCode::SUCCESS
            }
            Err(_) => fail("recovery manifest serialization failed"),
        },
        Err(message) => fail(&message),
    }
}

async fn run() -> Result<CompleteRecoveryManifest, String> {
    let rng_master_key =
        env::var("RNG_MASTER_KEY_FILE").unwrap_or_else(|_| "data/rng-master.key".to_owned());
    let image_artifact_root =
        env::var("IMAGE_ARTIFACT_ROOT").unwrap_or_else(|_| "data/generated-images".to_owned());
    let config = AppConfig::load().map_err(safe_error)?;
    let store = MongoStore::connect(&config.persistence.mongodb)
        .await
        .map_err(safe_error)?;
    let repository = MongoRepository::new(store);
    let database = repository
        .database_recovery_manifest(LOCAL_HERO_OWNER_KEY)
        .await
        .map_err(safe_error)?;
    CompleteRecoveryManifest::build(
        database,
        Path::new(&rng_master_key),
        Path::new(&image_artifact_root),
    )
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
