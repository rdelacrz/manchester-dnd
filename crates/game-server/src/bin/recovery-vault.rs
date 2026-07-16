//! Offline authenticated-encryption utility for complete recovery bundles.

use std::{env, path::Path, process::ExitCode, time::UNIX_EPOCH};

use manchester_dnd_server::recovery_vault::{
    create_recovery_key, expire_recovery_vaults, inspect_recovery_vault, open_recovery_vault,
    seal_recovery_bundle,
};
use serde_json::json;

fn main() -> ExitCode {
    match run() {
        Ok(value) => match serde_json::to_string_pretty(&json!({ "ok": value })) {
            Ok(body) => {
                println!("{body}");
                ExitCode::SUCCESS
            }
            Err(_) => fail("receipt serialization failed"),
        },
        Err(message) => fail(&message),
    }
}

fn run() -> Result<serde_json::Value, String> {
    let mut args = env::args_os();
    let _program = args.next();
    let operation = args
        .next()
        .and_then(|value| value.into_string().ok())
        .ok_or_else(usage)?;
    match operation.as_str() {
        "create-key" => {
            let key = args.next().ok_or_else(usage)?;
            ensure_end(&mut args)?;
            create_recovery_key(Path::new(&key)).map_err(safe_error)?;
            Ok(json!({ "created": true }))
        }
        "seal" => {
            let input = args.next().ok_or_else(usage)?;
            let vault = args.next().ok_or_else(usage)?;
            let key = args.next().ok_or_else(usage)?;
            ensure_end(&mut args)?;
            let now = current_epoch()?;
            serde_json::to_value(
                seal_recovery_bundle(Path::new(&input), Path::new(&vault), Path::new(&key), now)
                    .map_err(safe_error)?,
            )
            .map_err(|_| "receipt serialization failed".to_owned())
        }
        "open" => {
            let vault = args.next().ok_or_else(usage)?;
            let output = args.next().ok_or_else(usage)?;
            let key = args.next().ok_or_else(usage)?;
            ensure_end(&mut args)?;
            serde_json::to_value(
                open_recovery_vault(Path::new(&vault), Path::new(&output), Path::new(&key))
                    .map_err(safe_error)?,
            )
            .map_err(|_| "receipt serialization failed".to_owned())
        }
        "inspect" => {
            let vault = args.next().ok_or_else(usage)?;
            let key = args.next().ok_or_else(usage)?;
            ensure_end(&mut args)?;
            serde_json::to_value(
                inspect_recovery_vault(Path::new(&vault), Path::new(&key)).map_err(safe_error)?,
            )
            .map_err(|_| "receipt serialization failed".to_owned())
        }
        "expire" => {
            let root = args.next().ok_or_else(usage)?;
            let key = args.next().ok_or_else(usage)?;
            let now_epoch = args
                .next()
                .and_then(|value| value.into_string().ok())
                .ok_or_else(usage)?
                .parse::<u64>()
                .map_err(|_| "current epoch must be an unsigned integer".to_owned())?;
            ensure_end(&mut args)?;
            serde_json::to_value(
                expire_recovery_vaults(Path::new(&root), Path::new(&key), now_epoch)
                    .map_err(safe_error)?,
            )
            .map_err(|_| "receipt serialization failed".to_owned())
        }
        _ => Err(usage()),
    }
}

fn current_epoch() -> Result<u64, String> {
    std::time::SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "system time is before the Unix epoch".to_owned())
        .map(|duration| duration.as_secs())
}

fn ensure_end(args: &mut impl Iterator<Item = std::ffi::OsString>) -> Result<(), String> {
    if args.next().is_some() {
        Err(usage())
    } else {
        Ok(())
    }
}

fn safe_error(error: impl std::fmt::Display) -> String {
    error.to_string()
}

fn usage() -> String {
    "usage: recovery-vault create-key KEY | seal INPUT VAULT KEY | open VAULT OUTPUT KEY | inspect VAULT KEY | expire BACKUP_ROOT KEY NOW_EPOCH".to_owned()
}

fn fail(message: &str) -> ExitCode {
    eprintln!(
        "{}",
        serde_json::to_string_pretty(&json!({ "ok": false, "error": message }))
            .unwrap_or_else(|_| "{\"ok\":false,\"error\":\"command failed\"}".to_owned())
    );
    ExitCode::FAILURE
}
