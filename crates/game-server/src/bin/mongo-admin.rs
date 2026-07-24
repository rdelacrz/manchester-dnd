use std::process::ExitCode;

use manchester_dnd_server::{
    AppConfig, MongoStore, SchemaReconciler, SecretString, config::validate_mongodb_uri,
};

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("mongo-admin failed: {error}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let command = parse_command(std::env::args().skip(1))
        .map_err(|usage| std::io::Error::new(std::io::ErrorKind::InvalidInput, usage))?;
    let config = AppConfig::load()?;
    let mut mongo = config.persistence.mongodb.clone();
    if command == Command::SchemaApply
        && let Ok(schema_uri) = std::env::var("MONGODB_SCHEMA_URI")
    {
        validate_mongodb_uri(&schema_uri, config.access_mode)?;
        mongo.uri = SecretString::new(schema_uri);
    }
    let store = MongoStore::connect(&mongo).await?;
    let schema = SchemaReconciler::new(store);

    match command {
        Command::SchemaApply => {
            let report = schema.apply().await?;
            println!(
                "schema applied: {} collection(s) created, {} validator(s) updated, {} index(es) created, metadata_updated={}",
                report.created_collections,
                report.updated_validators,
                report.created_indexes,
                report.metadata_updated
            );
        }
        Command::SchemaVerify => {
            let report = schema.verify().await?;
            println!(
                "schema verified: {} collections, {} indexes, bundle v{} {}",
                report.collections, report.indexes, report.bundle_version, report.bundle_digest
            );
        }
        Command::IndexesVerify => {
            let count = schema.verify_indexes().await?;
            println!("indexes verified: {count} managed indexes");
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Command {
    SchemaApply,
    SchemaVerify,
    IndexesVerify,
}

fn parse_command(args: impl IntoIterator<Item = String>) -> Result<Command, &'static str> {
    let args = args.into_iter().collect::<Vec<_>>();
    match args.as_slice() {
        [group, action] if group == "schema" && action == "apply" => Ok(Command::SchemaApply),
        [group, action] if group == "schema" && action == "verify" => Ok(Command::SchemaVerify),
        [group, action] if group == "indexes" && action == "verify" => Ok(Command::IndexesVerify),
        _ => Err("usage: mongo-admin <schema apply|schema verify|indexes verify>"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_surface_is_closed() {
        assert_eq!(
            parse_command(["schema".to_owned(), "apply".to_owned()]).unwrap(),
            Command::SchemaApply
        );
        assert!(parse_command(["schema".to_owned(), "drop".to_owned()]).is_err());
        assert!(parse_command(["apply".to_owned()]).is_err());
    }
}
