use std::{process::ExitCode, time::Duration};

use manchester_dnd_server::{
    AppConfig, AuthService, CacheService, MongoAccountRepository, MongoSchemaPolicy, MongoStore,
    SchemaReconciler,
};

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("issue-signup-token failed: {error}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let arguments = parse_arguments(std::env::args().skip(1))
        .map_err(|usage| std::io::Error::new(std::io::ErrorKind::InvalidInput, usage))?;
    let config = AppConfig::load()?;
    let store = MongoStore::connect(&config.persistence.mongodb).await?;
    let schema = SchemaReconciler::new(store.clone());
    match config.persistence.mongodb.schema_policy {
        MongoSchemaPolicy::ApplyAndVerify => {
            schema.apply().await?;
        }
        MongoSchemaPolicy::VerifyOnly => {
            schema.verify().await?;
        }
    }
    let auth = AuthService::new(
        MongoAccountRepository::new(store),
        CacheService::disabled(),
        config.authentication,
    )?;
    let issued = auth
        .issue_signup_access_token("user", &arguments.issued_by, arguments.expires_in)
        .await?;

    eprintln!(
        "issued {} for role=user; expires {}. Raw token follows once on stdout.",
        issued.id, issued.expires_at
    );
    println!("{}", issued.token.expose_secret());
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
struct Arguments {
    expires_in: Duration,
    issued_by: String,
}

fn parse_arguments(args: impl IntoIterator<Item = String>) -> Result<Arguments, &'static str> {
    let args = args.into_iter().collect::<Vec<_>>();
    let mut expires_in = None;
    let mut issued_by = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--role" if args.get(index + 1).map(String::as_str) == Some("user") => {
                index += 2;
            }
            "--expires-in" => {
                let value = args.get(index + 1).ok_or(USAGE)?;
                expires_in = Some(parse_duration(value).ok_or(USAGE)?);
                index += 2;
            }
            "--issued-by" => {
                issued_by = Some(args.get(index + 1).ok_or(USAGE)?.clone());
                index += 2;
            }
            _ => return Err(USAGE),
        }
    }
    Ok(Arguments {
        expires_in: expires_in.ok_or(USAGE)?,
        issued_by: issued_by.unwrap_or_else(|| "operator:cli".to_owned()),
    })
}

fn parse_duration(value: &str) -> Option<Duration> {
    let (number, multiplier) = if let Some(days) = value.strip_suffix('d') {
        (days, 24 * 60 * 60)
    } else if let Some(hours) = value.strip_suffix('h') {
        (hours, 60 * 60)
    } else if let Some(minutes) = value.strip_suffix('m') {
        (minutes, 60)
    } else {
        (value, 1)
    };
    number
        .parse::<u64>()
        .ok()
        .and_then(|number| number.checked_mul(multiplier))
        .filter(|seconds| *seconds > 0)
        .map(Duration::from_secs)
}

const USAGE: &str = "usage: issue-signup-token --role user --expires-in <seconds|Nm|Nh|Nd> [--issued-by <opaque-id>]";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_requires_user_role_and_bounded_duration_syntax() {
        assert_eq!(
            parse_arguments([
                "--role".to_owned(),
                "user".to_owned(),
                "--expires-in".to_owned(),
                "7d".to_owned(),
            ])
            .unwrap(),
            Arguments {
                expires_in: Duration::from_secs(7 * 24 * 60 * 60),
                issued_by: "operator:cli".to_owned(),
            }
        );
        assert!(
            parse_arguments([
                "--role".to_owned(),
                "admin".to_owned(),
                "--expires-in".to_owned(),
                "7d".to_owned(),
            ])
            .is_err()
        );
        assert!(
            parse_arguments([
                "--role".to_owned(),
                "user".to_owned(),
                "--expires-in".to_owned(),
                "0".to_owned(),
            ])
            .is_err()
        );
    }
}
