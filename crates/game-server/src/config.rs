use std::{
    env, fmt,
    net::SocketAddr,
    path::{Path, PathBuf},
    str::FromStr,
    time::Duration,
};

use url::Url;

use crate::error::ConfigError;

const DEFAULT_DATABASE_URL: &str = "sqlite://data/manchester-arcana.db";
const DEFAULT_EVENT_PROMPTS_DIR: &str = "prompts/events/private";
const DEFAULT_TIMEOUT_SECONDS: u64 = 60;
const MAX_TIMEOUT_SECONDS: u64 = 600;
const MAX_OUTPUT_TOKENS: u32 = 128_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessMode {
    LocalSingleUser,
    Hosted,
}

impl FromStr for AccessMode {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "local" | "local-single-user" | "local_single_user" => Ok(Self::LocalSingleUser),
            "hosted" => Ok(Self::Hosted),
            _ => Err("expected local or hosted".to_owned()),
        }
    }
}

/// A secret whose `Debug` and `Display` implementations never reveal its value.
#[derive(Clone, PartialEq, Eq)]
pub struct SecretString(String);

impl SecretString {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub(crate) fn expose_secret(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SecretString {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SecretString([REDACTED])")
    }
}

impl fmt::Display for SecretString {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("[REDACTED]")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LlmBackend {
    Disabled,
    OpenAiCompatible,
}

impl FromStr for LlmBackend {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "disabled" | "none" | "off" => Ok(Self::Disabled),
            "openai" | "openai-compatible" | "openai_compatible" => Ok(Self::OpenAiCompatible),
            _ => Err("expected disabled or openai-compatible".to_owned()),
        }
    }
}

#[derive(Debug, Clone)]
pub struct LlmProfile {
    pub backend: LlmBackend,
    pub base_url: Option<Url>,
    pub api_key: Option<SecretString>,
    pub model: Option<String>,
    pub timeout: Duration,
    pub max_output_tokens: Option<u32>,
    pub temperature: Option<f32>,
    pub default_image_size: Option<String>,
}

impl LlmProfile {
    fn from_lookup(
        profile: &'static str,
        prefix: &'static str,
        get: &mut impl FnMut(&str) -> Option<String>,
    ) -> Result<Self, ConfigError> {
        let backend_name = env_name(prefix, "BACKEND");
        let backend = get(&backend_name)
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| "disabled".to_owned())
            .parse()
            .map_err(|reason| ConfigError::InvalidValue {
                name: profile_env_name(prefix, "BACKEND"),
                reason,
            })?;

        let base_url_name = env_name(prefix, "BASE_URL");
        let base_url = get(&base_url_name)
            .filter(|value| !value.trim().is_empty())
            .map(|value| parse_base_url(profile_env_name(prefix, "BASE_URL"), &value))
            .transpose()?;

        let api_key_name = env_name(prefix, "API_KEY");
        let api_key = get(&api_key_name)
            .filter(|value| !value.is_empty())
            .map(SecretString::new);

        let model_name = env_name(prefix, "MODEL");
        let model = get(&model_name).filter(|value| !value.trim().is_empty());
        if model
            .as_ref()
            .is_some_and(|value| value.chars().count() > 256)
        {
            return Err(ConfigError::InvalidValue {
                name: profile_env_name(prefix, "MODEL"),
                reason: "must contain at most 256 characters".to_owned(),
            });
        }

        let timeout_name = env_name(prefix, "TIMEOUT_SECONDS");
        let timeout_seconds = parse_optional::<u64>(
            get(&timeout_name),
            profile_env_name(prefix, "TIMEOUT_SECONDS"),
        )?
        .unwrap_or(DEFAULT_TIMEOUT_SECONDS);
        if !(1..=MAX_TIMEOUT_SECONDS).contains(&timeout_seconds) {
            return Err(ConfigError::InvalidValue {
                name: profile_env_name(prefix, "TIMEOUT_SECONDS"),
                reason: format!("must be between 1 and {MAX_TIMEOUT_SECONDS}"),
            });
        }

        let max_output_tokens = if prefix == "TEXT_LLM" {
            let name = env_name(prefix, "MAX_OUTPUT_TOKENS");
            let value =
                parse_optional::<u32>(get(&name), profile_env_name(prefix, "MAX_OUTPUT_TOKENS"))?;
            if value.is_some_and(|tokens| !(1..=MAX_OUTPUT_TOKENS).contains(&tokens)) {
                return Err(ConfigError::InvalidValue {
                    name: "TEXT_LLM_MAX_OUTPUT_TOKENS",
                    reason: format!("must be between 1 and {MAX_OUTPUT_TOKENS}"),
                });
            }
            value
        } else {
            None
        };

        let temperature = if prefix == "TEXT_LLM" {
            let name = env_name(prefix, "TEMPERATURE");
            let temperature =
                parse_optional::<f32>(get(&name), profile_env_name(prefix, "TEMPERATURE"))?;
            if temperature.is_some_and(|value| !value.is_finite() || !(0.0..=2.0).contains(&value))
            {
                return Err(ConfigError::InvalidValue {
                    name: "TEXT_LLM_TEMPERATURE",
                    reason: "must be finite and between 0 and 2".to_owned(),
                });
            }
            temperature
        } else {
            None
        };
        let default_image_size = if prefix == "IMAGE_LLM" {
            let name = env_name(prefix, "SIZE");
            let value = get(&name).filter(|value| !value.trim().is_empty());
            if value.as_ref().is_some_and(|size| {
                size.len() > 64
                    || !size.bytes().all(|byte| {
                        byte.is_ascii_alphanumeric() || matches!(byte, b'x' | b'X' | b'-' | b'_')
                    })
            }) {
                return Err(ConfigError::InvalidValue {
                    name: "IMAGE_LLM_SIZE",
                    reason: "must be a bounded provider size identifier".to_owned(),
                });
            }
            value
        } else {
            None
        };

        if backend == LlmBackend::OpenAiCompatible {
            if base_url.is_none() {
                return Err(ConfigError::MissingProfileValue {
                    profile,
                    name: profile_env_name(prefix, "BASE_URL"),
                });
            }
            if model.is_none() {
                return Err(ConfigError::MissingProfileValue {
                    profile,
                    name: profile_env_name(prefix, "MODEL"),
                });
            }
        }

        Ok(Self {
            backend,
            base_url,
            api_key,
            model,
            timeout: Duration::from_secs(timeout_seconds),
            max_output_tokens,
            temperature,
            default_image_size,
        })
    }
}

#[derive(Debug, Clone)]
pub struct AppConfig {
    pub access_mode: AccessMode,
    pub database_url: String,
    pub event_prompts_dir: PathBuf,
    pub text_llm: LlmProfile,
    pub image_llm: LlmProfile,
}

impl AppConfig {
    /// Loads an explicitly selected dotenv file when `APP_ENV_FILE` is set;
    /// otherwise, loads the nearest `.env` if one exists. Existing process
    /// environment values retain precedence, as provided by `dotenvy`.
    pub fn load() -> Result<Self, ConfigError> {
        match env::var_os("APP_ENV_FILE") {
            Some(path) if !path.is_empty() => load_dotenv_file(Path::new(&path))?,
            Some(_) => {
                return Err(ConfigError::InvalidValue {
                    name: "APP_ENV_FILE",
                    reason: "must not be empty".to_owned(),
                });
            }
            None => match dotenvy::dotenv() {
                Ok(_) => {}
                Err(dotenvy::Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(source) => {
                    return Err(ConfigError::Dotenv {
                        path: PathBuf::from(".env"),
                        reason: sanitized_dotenv_reason(&source),
                    });
                }
            },
        }

        Self::from_lookup(|name| env::var(name).ok())
    }

    /// Loads a specific dotenv file, then builds configuration from the process
    /// environment. This is useful for applications that select profiles before
    /// launching the server.
    pub fn load_file(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        load_dotenv_file(path.as_ref())?;
        Self::from_lookup(|name| env::var(name).ok())
    }

    fn from_lookup(mut get: impl FnMut(&str) -> Option<String>) -> Result<Self, ConfigError> {
        let access_mode = get("APP_ACCESS_MODE")
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| "local".to_owned())
            .parse()
            .map_err(|reason| ConfigError::InvalidValue {
                name: "APP_ACCESS_MODE",
                reason,
            })?;
        let database_url = get("DATABASE_URL")
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_DATABASE_URL.to_owned());
        if !database_url.starts_with("sqlite:") {
            return Err(ConfigError::InvalidValue {
                name: "DATABASE_URL",
                reason: "only sqlite: URLs are supported".to_owned(),
            });
        }

        let event_prompts_dir = get("EVENT_PROMPT_DIR")
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_EVENT_PROMPTS_DIR.to_owned())
            .into();

        let text_llm = LlmProfile::from_lookup("text", "TEXT_LLM", &mut get)?;
        let image_llm = LlmProfile::from_lookup("image", "IMAGE_LLM", &mut get)?;

        Ok(Self {
            access_mode,
            database_url,
            event_prompts_dir,
            text_llm,
            image_llm,
        })
    }

    /// Local single-user mode is an explicit deployment boundary, not an
    /// authentication substitute. Refuse to expose it on a non-loopback bind.
    pub fn validate_bind_address(&self, address: SocketAddr) -> Result<(), ConfigError> {
        self.validate_access_mode()?;
        match self.access_mode {
            AccessMode::LocalSingleUser if !address.ip().is_loopback() => {
                return Err(ConfigError::InvalidValue {
                    name: "LEPTOS_SITE_ADDR",
                    reason: "local access mode must bind to a loopback address".to_owned(),
                });
            }
            AccessMode::LocalSingleUser | AccessMode::Hosted => {}
        }
        Ok(())
    }

    pub fn validate_access_mode(&self) -> Result<(), ConfigError> {
        if self.access_mode == AccessMode::Hosted {
            return Err(ConfigError::InvalidValue {
                name: "APP_ACCESS_MODE",
                reason:
                    "hosted mode is unavailable until authenticated browser sessions are implemented"
                        .to_owned(),
            });
        }
        Ok(())
    }
}

fn load_dotenv_file(path: &Path) -> Result<(), ConfigError> {
    dotenvy::from_path(path)
        .map(|_| ())
        .map_err(|source| ConfigError::Dotenv {
            path: path.to_owned(),
            reason: sanitized_dotenv_reason(&source),
        })
}

fn sanitized_dotenv_reason(error: &dotenvy::Error) -> &'static str {
    match error {
        dotenvy::Error::Io(_) => "the file could not be read",
        dotenvy::Error::LineParse(_, _) => "the file contains a malformed line",
        dotenvy::Error::EnvVar(_) => "an environment value could not be resolved",
        _ => "the environment file could not be loaded",
    }
}

fn parse_base_url(name: &'static str, raw: &str) -> Result<Url, ConfigError> {
    let url = Url::parse(raw).map_err(|error| ConfigError::InvalidValue {
        name,
        reason: error.to_string(),
    })?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(ConfigError::InvalidValue {
            name,
            reason: "scheme must be http or https".to_owned(),
        });
    }
    if url.scheme() == "http"
        && !url.host().is_some_and(|host| match host {
            url::Host::Domain(domain) => domain.eq_ignore_ascii_case("localhost"),
            url::Host::Ipv4(address) => address.is_loopback(),
            url::Host::Ipv6(address) => address.is_loopback(),
        })
    {
        return Err(ConfigError::InvalidValue {
            name,
            reason: "non-local model endpoints must use HTTPS".to_owned(),
        });
    }
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(ConfigError::InvalidValue {
            name,
            reason: "credentials, query strings, and fragments are not allowed".to_owned(),
        });
    }
    Ok(url)
}

fn parse_optional<T>(value: Option<String>, name: &'static str) -> Result<Option<T>, ConfigError>
where
    T: FromStr,
    T::Err: fmt::Display,
{
    value
        .filter(|value| !value.trim().is_empty())
        .map(|value| {
            value
                .parse::<T>()
                .map_err(|error| ConfigError::InvalidValue {
                    name,
                    reason: error.to_string(),
                })
        })
        .transpose()
}

fn env_name(prefix: &str, suffix: &str) -> String {
    format!("{prefix}_{suffix}")
}

fn profile_env_name(prefix: &'static str, suffix: &'static str) -> &'static str {
    match (prefix, suffix) {
        ("TEXT_LLM", "BACKEND") => "TEXT_LLM_BACKEND",
        ("TEXT_LLM", "BASE_URL") => "TEXT_LLM_BASE_URL",
        ("TEXT_LLM", "MODEL") => "TEXT_LLM_MODEL",
        ("TEXT_LLM", "TIMEOUT_SECONDS") => "TEXT_LLM_TIMEOUT_SECONDS",
        ("TEXT_LLM", "MAX_OUTPUT_TOKENS") => "TEXT_LLM_MAX_OUTPUT_TOKENS",
        ("TEXT_LLM", "TEMPERATURE") => "TEXT_LLM_TEMPERATURE",
        ("IMAGE_LLM", "BACKEND") => "IMAGE_LLM_BACKEND",
        ("IMAGE_LLM", "BASE_URL") => "IMAGE_LLM_BASE_URL",
        ("IMAGE_LLM", "MODEL") => "IMAGE_LLM_MODEL",
        ("IMAGE_LLM", "TIMEOUT_SECONDS") => "IMAGE_LLM_TIMEOUT_SECONDS",
        ("IMAGE_LLM", "SIZE") => "IMAGE_LLM_SIZE",
        _ => unreachable!("profile environment names are defined statically"),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    #[test]
    fn disabled_profiles_need_no_provider_values() {
        let config = AppConfig::from_lookup(|_| None).expect("defaults should be valid");

        assert_eq!(config.access_mode, AccessMode::LocalSingleUser);
        assert_eq!(config.text_llm.backend, LlmBackend::Disabled);
        assert_eq!(config.image_llm.backend, LlmBackend::Disabled);
    }

    #[test]
    fn local_mode_requires_a_loopback_bind() {
        let config = AppConfig::from_lookup(|_| None).expect("defaults should be valid");

        assert!(
            config
                .validate_bind_address("127.0.0.1:6789".parse().unwrap())
                .is_ok()
        );
        assert!(
            config
                .validate_bind_address("0.0.0.0:6789".parse().unwrap())
                .is_err()
        );
    }

    #[test]
    fn hosted_mode_fails_closed_until_authentication_exists() {
        let values = HashMap::from([("APP_ACCESS_MODE", "hosted")]);
        let config = AppConfig::from_lookup(|name| values.get(name).map(ToString::to_string))
            .expect("hosted mode should parse");

        assert_eq!(config.access_mode, AccessMode::Hosted);
        assert!(
            config
                .validate_bind_address("0.0.0.0:6789".parse().unwrap())
                .is_err()
        );
        assert!(
            config
                .validate_bind_address("127.0.0.1:6789".parse().unwrap())
                .is_err()
        );
    }

    #[test]
    fn profiles_are_independently_configurable_and_secrets_are_redacted() {
        let values = HashMap::from([
            ("TEXT_LLM_BACKEND", "openai-compatible"),
            ("TEXT_LLM_BASE_URL", "https://text.example.test/v1/"),
            ("TEXT_LLM_API_KEY", "text-super-secret"),
            ("TEXT_LLM_MODEL", "narrator"),
            ("IMAGE_LLM_BACKEND", "openai"),
            ("IMAGE_LLM_BASE_URL", "https://images.example.test/api/"),
            ("IMAGE_LLM_API_KEY", "image-super-secret"),
            ("IMAGE_LLM_MODEL", "illustrator"),
        ]);

        let config = AppConfig::from_lookup(|name| values.get(name).map(ToString::to_string))
            .expect("profiles should parse");
        let debug = format!("{config:?}");

        assert_eq!(config.text_llm.model.as_deref(), Some("narrator"));
        assert_eq!(config.image_llm.model.as_deref(), Some("illustrator"));
        assert!(!debug.contains("text-super-secret"));
        assert!(!debug.contains("image-super-secret"));
        assert!(debug.contains("[REDACTED]"));
    }

    #[test]
    fn enabled_profile_requires_endpoint_and_model() {
        let values = HashMap::from([("TEXT_LLM_BACKEND", "openai")]);
        let error = AppConfig::from_lookup(|name| values.get(name).map(ToString::to_string))
            .expect_err("missing endpoint must fail");

        assert!(matches!(
            error,
            ConfigError::MissingProfileValue {
                name: "TEXT_LLM_BASE_URL",
                ..
            }
        ));
    }

    #[test]
    fn remote_plaintext_model_endpoints_are_rejected() {
        assert!(parse_base_url("TEXT_LLM_BASE_URL", "http://example.test/v1").is_err());
        assert!(parse_base_url("TEXT_LLM_BASE_URL", "http://127.0.0.1:11434/v1").is_ok());
        assert!(parse_base_url("TEXT_LLM_BASE_URL", "https://example.test/v1").is_ok());
    }
}
