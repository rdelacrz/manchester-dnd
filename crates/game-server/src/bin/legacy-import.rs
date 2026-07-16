//! Explicit one-time import from the supported immutable SQLite v2 database.

use std::{env, path::Path, process::ExitCode};

use manchester_dnd_core::Sha256Digest;
use manchester_dnd_server::{
    DatabaseRuntimeConfig, import_legacy_sqlite, repository::PostgresRepository,
};
use serde_json::json;

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(report) => match serde_json::to_string_pretty(&report) {
            Ok(body) => {
                println!("{body}");
                ExitCode::SUCCESS
            }
            Err(_) => fail("legacy import report serialization failed"),
        },
        Err(message) => fail(&message),
    }
}

async fn run() -> Result<manchester_dnd_server::LegacyImportReport, String> {
    let mut args = env::args_os();
    let _program = args.next();
    let source = args.next().ok_or_else(usage)?;
    let expected_digest = args
        .next()
        .and_then(|value| value.into_string().ok())
        .ok_or_else(usage)?;
    if args.next().is_some() {
        return Err(usage());
    }
    let expected_digest = Sha256Digest::new(expected_digest)
        .map_err(|_| "expected source digest must be a SHA-256 digest".to_owned())?;
    let database_url = env::var("DATABASE_URL")
        .map_err(|_| "DATABASE_URL is required for legacy import".to_owned())?;
    if database_url.trim().is_empty() {
        return Err("DATABASE_URL is required for legacy import".to_owned());
    }
    let runtime = DatabaseRuntimeConfig {
        max_connections: 1,
        migrate_on_start: false,
        ..DatabaseRuntimeConfig::default()
    };
    let repository = PostgresRepository::connect(&database_url, runtime)
        .await
        .map_err(safe_error)?;
    import_legacy_sqlite(&repository, Path::new(&source), &expected_digest)
        .await
        .map_err(safe_error)
}

fn safe_error(error: impl std::fmt::Display) -> String {
    error.to_string()
}

fn usage() -> String {
    "usage: legacy-import SQLITE_DATABASE EXPECTED_SHA256_DIGEST".to_owned()
}

fn fail(message: &str) -> ExitCode {
    eprintln!(
        "{}",
        serde_json::to_string_pretty(&json!({ "ok": false, "error": message }))
            .unwrap_or_else(|_| "{\"ok\":false,\"error\":\"command failed\"}".to_owned())
    );
    ExitCode::FAILURE
}
