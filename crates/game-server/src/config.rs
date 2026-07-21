use std::{
    env, fmt,
    net::SocketAddr,
    path::{Component, Path, PathBuf},
    str::FromStr,
    time::Duration,
};

use manchester_dnd_core::{
    Sha256Digest,
    hero::{EMBERLINE_THEME_PACK_ID, RAINBOUND_THEME_PACK_ID},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use url::Url;

use crate::error::ConfigError;

const DEFAULT_DATABASE_URL: &str = "postgresql://127.0.0.1/manchester_arcana";
const DEFAULT_DATABASE_MAX_CONNECTIONS: u32 = 10;
const DEFAULT_DATABASE_ACQUIRE_TIMEOUT_MILLISECONDS: u64 = 5_000;
const DEFAULT_DATABASE_STATEMENT_TIMEOUT_MILLISECONDS: u64 = 30_000;
const DEFAULT_DATABASE_LOCK_TIMEOUT_MILLISECONDS: u64 = 5_000;
const DEFAULT_DATABASE_IDLE_TRANSACTION_TIMEOUT_MILLISECONDS: u64 = 15_000;
const DEFAULT_CONTENT_PACK_ROOT: &str = "content/packs";
const DEFAULT_CONTENT_THEME_PACK_ID: &str = RAINBOUND_THEME_PACK_ID;
const DEFAULT_EVENT_PROMPTS_DIR: &str = "prompts/events/private";
const DEFAULT_IMAGE_ARTIFACT_ROOT: &str = "data/generated-images";
const DEFAULT_RNG_MASTER_KEY_FILE: &str = "data/rng-master.key";
const DEFAULT_TIMEOUT_SECONDS: u64 = 60;
const MAX_TIMEOUT_SECONDS: u64 = 600;
const MAX_OUTPUT_TOKENS: u32 = 128_000;
const DEFAULT_CAMPAIGN_GENERATION_REQUEST_BUDGET: u64 = 256;
const DEFAULT_TURN_GENERATION_REQUEST_BUDGET: u64 = 8;
const DEFAULT_CAMPAIGN_GENERATION_TOKEN_BUDGET: u64 = 4_000_000;
const DEFAULT_TURN_GENERATION_TOKEN_BUDGET: u64 = 512_000;
const DEFAULT_CAMPAIGN_GENERATION_LATENCY_BUDGET_MILLISECONDS: u64 = 3_600_000;
const DEFAULT_TURN_GENERATION_LATENCY_BUDGET_MILLISECONDS: u64 = 600_000;
const DEFAULT_MAX_CAMPAIGN_GENERATION_CONCURRENCY: u16 = 2;
const DEFAULT_GENERATION_WORKER_BATCH_SIZE: u16 = 4;

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
    Fake,
    OpenAiCompatible,
}

impl FromStr for LlmBackend {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "disabled" | "none" | "off" => Ok(Self::Disabled),
            "fake" | "deterministic-fake" | "deterministic_fake" => Ok(Self::Fake),
            "openai" | "openai-compatible" | "openai_compatible" => Ok(Self::OpenAiCompatible),
            _ => Err("expected disabled, fake, or openai-compatible".to_owned()),
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
    /// Operator estimate charged by durable preflight before provider work.
    /// Disabled and deterministic-fake profiles default explicitly to zero.
    pub estimated_request_cost_microusd: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GenerationBudgetAllowance {
    pub requests: u64,
    pub tokens: u64,
    pub latency_milliseconds: u64,
    pub cost_microusd: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenerationGovernanceConfig {
    pub campaign: GenerationBudgetAllowance,
    pub turn: GenerationBudgetAllowance,
    pub max_campaign_concurrency: u16,
    pub worker_batch_size: u16,
}

impl GenerationGovernanceConfig {
    #[must_use]
    pub fn non_secret_fingerprint(&self) -> Sha256Digest {
        let mut hasher = Sha256::new();
        for value in [
            "generation-governance/v1".to_owned(),
            self.campaign.requests.to_string(),
            self.campaign.tokens.to_string(),
            self.campaign.latency_milliseconds.to_string(),
            self.campaign.cost_microusd.to_string(),
            self.turn.requests.to_string(),
            self.turn.tokens.to_string(),
            self.turn.latency_milliseconds.to_string(),
            self.turn.cost_microusd.to_string(),
            self.max_campaign_concurrency.to_string(),
            self.worker_batch_size.to_string(),
        ] {
            hash_fingerprint_field(&mut hasher, &value);
        }
        Sha256Digest::from_bytes(hasher.finalize().into())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GenerationConfigFingerprints {
    pub text: Sha256Digest,
    pub image: Sha256Digest,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DatabaseRuntimeConfig {
    pub max_connections: u32,
    pub acquire_timeout: Duration,
    pub statement_timeout: Duration,
    pub lock_timeout: Duration,
    pub idle_transaction_timeout: Duration,
    pub migrate_on_start: bool,
}

impl Default for DatabaseRuntimeConfig {
    fn default() -> Self {
        Self {
            max_connections: DEFAULT_DATABASE_MAX_CONNECTIONS,
            acquire_timeout: Duration::from_millis(DEFAULT_DATABASE_ACQUIRE_TIMEOUT_MILLISECONDS),
            statement_timeout: Duration::from_millis(
                DEFAULT_DATABASE_STATEMENT_TIMEOUT_MILLISECONDS,
            ),
            lock_timeout: Duration::from_millis(DEFAULT_DATABASE_LOCK_TIMEOUT_MILLISECONDS),
            idle_transaction_timeout: Duration::from_millis(
                DEFAULT_DATABASE_IDLE_TRANSACTION_TIMEOUT_MILLISECONDS,
            ),
            migrate_on_start: true,
        }
    }
}

/// Authentication, session, and throttling configuration.
///
/// In hosted mode, `cookie_secure` must be `true` and `canonical_origin` must
/// be set. Local mode permits `cookie_secure = false` for loopback development.
#[derive(Debug, Clone)]
pub struct AuthenticationConfig {
    pub session_idle_lifetime: Duration,
    pub session_absolute_lifetime: Duration,
    pub max_active_sessions: u32,
    pub max_hash_concurrency: usize,
    pub throttle_window_seconds: u64,
    pub throttle_block_after_attempts: u32,
    pub throttle_block_seconds: u64,
    pub throttle_hmac_key: SecretString,
    pub cookie_secure: bool,
    pub canonical_origin: Option<String>,
    pub argon2_memory_kib: u32,
    pub argon2_iterations: u32,
    pub argon2_parallelism: u32,
}

impl Default for AuthenticationConfig {
    fn default() -> Self {
        Self {
            session_idle_lifetime: Duration::from_secs(60 * 60 * 24 * 7),
            session_absolute_lifetime: Duration::from_secs(60 * 60 * 24 * 30),
            max_active_sessions: 5,
            max_hash_concurrency: 2,
            throttle_window_seconds: 300,
            throttle_block_after_attempts: 5,
            throttle_block_seconds: 60,
            throttle_hmac_key: SecretString::new("change-me-throttle-hmac-key"),
            cookie_secure: false,
            canonical_origin: None,
            argon2_memory_kib: 19_456,
            argon2_iterations: 2,
            argon2_parallelism: 1,
        }
    }
}

/// Trusted deployment selection for the fixed private-MVP content allowlist.
/// The root may move between source checkouts and runtime images, but startup
/// only inspects the three compiled pack directories beneath it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContentPackConfig {
    pub root: PathBuf,
    pub default_theme_pack_id: String,
}

impl LlmProfile {
    /// Produces a stable deployment fingerprint without including credentials.
    /// Retained generated output stores this value, never the source profile.
    pub fn non_secret_fingerprint(&self, capability: &str) -> Sha256Digest {
        let mut hasher = Sha256::new();
        hash_fingerprint_field(&mut hasher, "generation-profile/v1");
        hash_fingerprint_field(&mut hasher, capability);
        hash_fingerprint_field(
            &mut hasher,
            match self.backend {
                LlmBackend::Disabled => "disabled",
                LlmBackend::Fake => "fake",
                LlmBackend::OpenAiCompatible => "openai-compatible",
            },
        );
        hash_fingerprint_field(&mut hasher, self.base_url.as_ref().map_or("", Url::as_str));
        hash_fingerprint_field(&mut hasher, self.model.as_deref().unwrap_or(""));
        hash_fingerprint_field(&mut hasher, &self.timeout.as_secs().to_string());
        hash_fingerprint_field(
            &mut hasher,
            &self.max_output_tokens.unwrap_or_default().to_string(),
        );
        hash_fingerprint_field(
            &mut hasher,
            &self.temperature.unwrap_or_default().to_bits().to_string(),
        );
        hash_fingerprint_field(
            &mut hasher,
            self.default_image_size.as_deref().unwrap_or(""),
        );
        hash_fingerprint_field(
            &mut hasher,
            &self.estimated_request_cost_microusd.to_string(),
        );
        Sha256Digest::from_bytes(hasher.finalize().into())
    }

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

        let estimated_cost_name = env_name(prefix, "ESTIMATED_REQUEST_COST_MICROUSD");
        let configured_estimated_cost = parse_optional::<u64>(
            get(&estimated_cost_name),
            profile_env_name(prefix, "ESTIMATED_REQUEST_COST_MICROUSD"),
        )?;

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
            if configured_estimated_cost.is_none() {
                return Err(ConfigError::MissingProfileValue {
                    profile,
                    name: profile_env_name(prefix, "ESTIMATED_REQUEST_COST_MICROUSD"),
                });
            }
            if prefix == "IMAGE_LLM" && configured_estimated_cost == Some(0) {
                return Err(ConfigError::InvalidValue {
                    name: "IMAGE_LLM_ESTIMATED_REQUEST_COST_MICROUSD",
                    reason: "must be greater than zero for a paid-capable image provider"
                        .to_owned(),
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
            estimated_request_cost_microusd: configured_estimated_cost.unwrap_or(0),
        })
    }

    #[must_use]
    pub fn estimated_request_tokens(&self) -> u64 {
        match self.backend {
            LlmBackend::Disabled => 0,
            LlmBackend::Fake => u64::from(self.max_output_tokens.unwrap_or(2_048)).max(4_096),
            LlmBackend::OpenAiCompatible => {
                u64::from(self.max_output_tokens.unwrap_or(2_048)).saturating_add(32_768)
            }
        }
    }

    #[must_use]
    pub fn estimated_request_latency_milliseconds(&self) -> u64 {
        match self.backend {
            LlmBackend::Disabled => 0,
            LlmBackend::Fake => 100,
            LlmBackend::OpenAiCompatible => self.timeout.as_millis().try_into().unwrap_or(u64::MAX),
        }
    }

    #[must_use]
    pub const fn estimated_request_units(&self) -> u64 {
        match self.backend {
            LlmBackend::Disabled => 0,
            LlmBackend::Fake | LlmBackend::OpenAiCompatible => 1,
        }
    }
}

#[derive(Debug, Clone)]
pub struct AppConfig {
    pub access_mode: AccessMode,
    pub database_url: SecretString,
    pub database: DatabaseRuntimeConfig,
    pub content_packs: ContentPackConfig,
    /// Deployment-wide private-inspiration gate. Campaign policy is an
    /// independent, narrower gate and may never override this value.
    pub inspiration_enabled: bool,
    pub event_prompts_dir: PathBuf,
    /// Protected, non-public storage for validated scene-image artifacts and
    /// short-lived quarantine bytes. It must never point into web assets.
    pub image_artifact_root: PathBuf,
    pub rng_master_key_file: PathBuf,
    pub text_llm: LlmProfile,
    pub image_llm: LlmProfile,
    pub generation_governance: GenerationGovernanceConfig,
    pub authentication: AuthenticationConfig,
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
        if !matches!(
            Url::parse(&database_url).ok().map(|url| url.scheme().to_owned()),
            Some(scheme) if matches!(scheme.as_str(), "postgres" | "postgresql")
        ) {
            return Err(ConfigError::InvalidValue {
                name: "DATABASE_URL",
                reason: "must be a valid postgres:// or postgresql:// URL".to_owned(),
            });
        }
        let database = DatabaseRuntimeConfig {
            max_connections: parse_generation_limit(
                &mut get,
                "DATABASE_MAX_CONNECTIONS",
                DEFAULT_DATABASE_MAX_CONNECTIONS,
            )?,
            acquire_timeout: Duration::from_millis(parse_generation_limit(
                &mut get,
                "DATABASE_ACQUIRE_TIMEOUT_MILLISECONDS",
                DEFAULT_DATABASE_ACQUIRE_TIMEOUT_MILLISECONDS,
            )?),
            statement_timeout: Duration::from_millis(parse_generation_limit(
                &mut get,
                "DATABASE_STATEMENT_TIMEOUT_MILLISECONDS",
                DEFAULT_DATABASE_STATEMENT_TIMEOUT_MILLISECONDS,
            )?),
            lock_timeout: Duration::from_millis(parse_generation_limit(
                &mut get,
                "DATABASE_LOCK_TIMEOUT_MILLISECONDS",
                DEFAULT_DATABASE_LOCK_TIMEOUT_MILLISECONDS,
            )?),
            idle_transaction_timeout: Duration::from_millis(parse_generation_limit(
                &mut get,
                "DATABASE_IDLE_TRANSACTION_TIMEOUT_MILLISECONDS",
                DEFAULT_DATABASE_IDLE_TRANSACTION_TIMEOUT_MILLISECONDS,
            )?),
            migrate_on_start: get("DATABASE_MIGRATE_ON_START")
                .filter(|value| !value.trim().is_empty())
                .map(|value| {
                    value
                        .parse::<bool>()
                        .map_err(|_| ConfigError::InvalidValue {
                            name: "DATABASE_MIGRATE_ON_START",
                            reason: "must be true or false".to_owned(),
                        })
                })
                .transpose()?
                .unwrap_or(true),
        };
        if !(1..=50).contains(&database.max_connections) {
            return Err(ConfigError::InvalidValue {
                name: "DATABASE_MAX_CONNECTIONS",
                reason: "must be between 1 and 50".to_owned(),
            });
        }
        for (name, value, maximum) in [
            (
                "DATABASE_ACQUIRE_TIMEOUT_MILLISECONDS",
                database.acquire_timeout,
                Duration::from_secs(60),
            ),
            (
                "DATABASE_STATEMENT_TIMEOUT_MILLISECONDS",
                database.statement_timeout,
                Duration::from_secs(300),
            ),
            (
                "DATABASE_LOCK_TIMEOUT_MILLISECONDS",
                database.lock_timeout,
                Duration::from_secs(60),
            ),
            (
                "DATABASE_IDLE_TRANSACTION_TIMEOUT_MILLISECONDS",
                database.idle_transaction_timeout,
                Duration::from_secs(300),
            ),
        ] {
            if value.is_zero() || value > maximum {
                return Err(ConfigError::InvalidValue {
                    name,
                    reason: format!("must be between 1 and {} milliseconds", maximum.as_millis()),
                });
            }
        }

        let event_prompts_dir = get("EVENT_PROMPT_DIR")
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_EVENT_PROMPTS_DIR.to_owned());
        let event_prompts_dir =
            parse_bounded_directory_path("EVENT_PROMPT_DIR", &event_prompts_dir)?;
        let image_artifact_root = get("IMAGE_ARTIFACT_ROOT")
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_IMAGE_ARTIFACT_ROOT.to_owned());
        let image_artifact_root =
            parse_bounded_directory_path("IMAGE_ARTIFACT_ROOT", &image_artifact_root)?;
        if image_artifact_root
            .components()
            .any(|component| matches!(component.as_os_str().to_str(), Some("public" | "target")))
        {
            return Err(ConfigError::InvalidValue {
                name: "IMAGE_ARTIFACT_ROOT",
                reason: "must not be inside a public or build-output directory".to_owned(),
            });
        }
        let inspiration_enabled = get("INSPIRATION_ENABLED")
            .filter(|value| !value.trim().is_empty())
            .map(|value| {
                value
                    .parse::<bool>()
                    .map_err(|_| ConfigError::InvalidValue {
                        name: "INSPIRATION_ENABLED",
                        reason: "must be true or false".to_owned(),
                    })
            })
            .transpose()?
            .unwrap_or(false);
        let rng_master_key_file = get("RNG_MASTER_KEY_FILE")
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_RNG_MASTER_KEY_FILE.to_owned())
            .into();
        let content_pack_root = get("CONTENT_PACK_ROOT")
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| DEFAULT_CONTENT_PACK_ROOT.to_owned());
        let content_pack_root = parse_content_pack_root(&content_pack_root)?;
        let default_theme_pack_id = get("CONTENT_DEFAULT_THEME_PACK_ID")
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_CONTENT_THEME_PACK_ID.to_owned());
        if !matches!(
            default_theme_pack_id.as_str(),
            RAINBOUND_THEME_PACK_ID | EMBERLINE_THEME_PACK_ID
        ) {
            return Err(ConfigError::InvalidValue {
                name: "CONTENT_DEFAULT_THEME_PACK_ID",
                reason: "must name one of the two bundled presentation theme packs".to_owned(),
            });
        }

        let text_llm = LlmProfile::from_lookup("text", "TEXT_LLM", &mut get)?;
        let image_llm = LlmProfile::from_lookup("image", "IMAGE_LLM", &mut get)?;
        let generation_governance = GenerationGovernanceConfig {
            campaign: GenerationBudgetAllowance {
                requests: parse_generation_limit(
                    &mut get,
                    "GENERATION_CAMPAIGN_REQUEST_BUDGET",
                    DEFAULT_CAMPAIGN_GENERATION_REQUEST_BUDGET,
                )?,
                tokens: parse_generation_limit(
                    &mut get,
                    "GENERATION_CAMPAIGN_TOKEN_BUDGET",
                    DEFAULT_CAMPAIGN_GENERATION_TOKEN_BUDGET,
                )?,
                latency_milliseconds: parse_generation_limit(
                    &mut get,
                    "GENERATION_CAMPAIGN_LATENCY_BUDGET_MILLISECONDS",
                    DEFAULT_CAMPAIGN_GENERATION_LATENCY_BUDGET_MILLISECONDS,
                )?,
                cost_microusd: parse_generation_limit(
                    &mut get,
                    "GENERATION_CAMPAIGN_COST_BUDGET_MICROUSD",
                    0,
                )?,
            },
            turn: GenerationBudgetAllowance {
                requests: parse_generation_limit(
                    &mut get,
                    "GENERATION_TURN_REQUEST_BUDGET",
                    DEFAULT_TURN_GENERATION_REQUEST_BUDGET,
                )?,
                tokens: parse_generation_limit(
                    &mut get,
                    "GENERATION_TURN_TOKEN_BUDGET",
                    DEFAULT_TURN_GENERATION_TOKEN_BUDGET,
                )?,
                latency_milliseconds: parse_generation_limit(
                    &mut get,
                    "GENERATION_TURN_LATENCY_BUDGET_MILLISECONDS",
                    DEFAULT_TURN_GENERATION_LATENCY_BUDGET_MILLISECONDS,
                )?,
                cost_microusd: parse_generation_limit(
                    &mut get,
                    "GENERATION_TURN_COST_BUDGET_MICROUSD",
                    0,
                )?,
            },
            max_campaign_concurrency: parse_generation_limit::<u16>(
                &mut get,
                "GENERATION_MAX_CAMPAIGN_CONCURRENCY",
                DEFAULT_MAX_CAMPAIGN_GENERATION_CONCURRENCY,
            )?,
            worker_batch_size: parse_generation_limit::<u16>(
                &mut get,
                "GENERATION_WORKER_BATCH_SIZE",
                DEFAULT_GENERATION_WORKER_BATCH_SIZE,
            )?,
        };
        if generation_governance.max_campaign_concurrency == 0
            || generation_governance.max_campaign_concurrency > 32
        {
            return Err(ConfigError::InvalidValue {
                name: "GENERATION_MAX_CAMPAIGN_CONCURRENCY",
                reason: "must be between 1 and 32".to_owned(),
            });
        }
        if generation_governance.worker_batch_size == 0
            || generation_governance.worker_batch_size > 100
        {
            return Err(ConfigError::InvalidValue {
                name: "GENERATION_WORKER_BATCH_SIZE",
                reason: "must be between 1 and 100".to_owned(),
            });
        }
        for (name, turn, campaign) in [
            (
                "GENERATION_TURN_REQUEST_BUDGET",
                generation_governance.turn.requests,
                generation_governance.campaign.requests,
            ),
            (
                "GENERATION_TURN_TOKEN_BUDGET",
                generation_governance.turn.tokens,
                generation_governance.campaign.tokens,
            ),
            (
                "GENERATION_TURN_LATENCY_BUDGET_MILLISECONDS",
                generation_governance.turn.latency_milliseconds,
                generation_governance.campaign.latency_milliseconds,
            ),
            (
                "GENERATION_TURN_COST_BUDGET_MICROUSD",
                generation_governance.turn.cost_microusd,
                generation_governance.campaign.cost_microusd,
            ),
        ] {
            if turn > campaign {
                return Err(ConfigError::InvalidValue {
                    name,
                    reason: "must not exceed the corresponding campaign budget".to_owned(),
                });
            }
        }

        Ok(Self {
            access_mode,
            database_url: SecretString::new(database_url),
            database,
            content_packs: ContentPackConfig {
                root: content_pack_root,
                default_theme_pack_id,
            },
            inspiration_enabled,
            event_prompts_dir,
            image_artifact_root,
            rng_master_key_file,
            text_llm,
            image_llm,
            generation_governance,
            authentication: AuthenticationConfig::default(),
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

    pub fn generation_config_fingerprints(&self) -> GenerationConfigFingerprints {
        GenerationConfigFingerprints {
            text: self.text_llm.non_secret_fingerprint("text"),
            image: self.image_llm.non_secret_fingerprint("image"),
        }
    }
}

fn parse_content_pack_root(raw: &str) -> Result<PathBuf, ConfigError> {
    parse_bounded_directory_path("CONTENT_PACK_ROOT", raw)
}

fn parse_bounded_directory_path(name: &'static str, raw: &str) -> Result<PathBuf, ConfigError> {
    let path = Path::new(raw);
    if raw.trim() != raw
        || raw.is_empty()
        || raw.chars().count() > 4_096
        || raw.contains('\0')
        || raw
            .split(['/', '\\'])
            .any(|segment| matches!(segment, "." | ".."))
        || path
            .components()
            .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
    {
        return Err(ConfigError::InvalidValue {
            name,
            reason: "must be a bounded normalized directory path without . or .. components"
                .to_owned(),
        });
    }
    Ok(path.to_owned())
}

fn hash_fingerprint_field(hasher: &mut Sha256, value: &str) {
    hasher.update(u64::try_from(value.len()).unwrap_or(u64::MAX).to_le_bytes());
    hasher.update(value.as_bytes());
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
    if url.host().is_some_and(|host| match host {
        url::Host::Ipv4(address) => !address.is_loopback(),
        url::Host::Ipv6(address) => !address.is_loopback(),
        url::Host::Domain(_) => false,
    }) {
        return Err(ConfigError::InvalidValue {
            name,
            reason: "direct IP provider endpoints are forbidden except loopback development"
                .to_owned(),
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

fn parse_generation_limit<T>(
    get: &mut impl FnMut(&str) -> Option<String>,
    name: &'static str,
    default: T,
) -> Result<T, ConfigError>
where
    T: FromStr,
    T::Err: fmt::Display,
{
    Ok(parse_optional(get(name), name)?.unwrap_or(default))
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
        ("TEXT_LLM", "ESTIMATED_REQUEST_COST_MICROUSD") => {
            "TEXT_LLM_ESTIMATED_REQUEST_COST_MICROUSD"
        }
        ("IMAGE_LLM", "BACKEND") => "IMAGE_LLM_BACKEND",
        ("IMAGE_LLM", "BASE_URL") => "IMAGE_LLM_BASE_URL",
        ("IMAGE_LLM", "MODEL") => "IMAGE_LLM_MODEL",
        ("IMAGE_LLM", "TIMEOUT_SECONDS") => "IMAGE_LLM_TIMEOUT_SECONDS",
        ("IMAGE_LLM", "SIZE") => "IMAGE_LLM_SIZE",
        ("IMAGE_LLM", "ESTIMATED_REQUEST_COST_MICROUSD") => {
            "IMAGE_LLM_ESTIMATED_REQUEST_COST_MICROUSD"
        }
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
        assert_eq!(config.content_packs.root, Path::new("content/packs"));
        assert_eq!(
            config.content_packs.default_theme_pack_id,
            RAINBOUND_THEME_PACK_ID
        );
        assert_eq!(config.text_llm.backend, LlmBackend::Disabled);
        assert_eq!(config.image_llm.backend, LlmBackend::Disabled);
        assert_eq!(
            config.image_artifact_root,
            Path::new("data/generated-images")
        );
        assert!(!config.inspiration_enabled);
    }

    #[test]
    fn database_pool_and_timeout_controls_are_bounded_and_field_specific() {
        let configured = HashMap::from([
            ("DATABASE_MIGRATE_ON_START", "false"),
            ("DATABASE_MAX_CONNECTIONS", "7"),
            ("DATABASE_ACQUIRE_TIMEOUT_MILLISECONDS", "4000"),
            ("DATABASE_STATEMENT_TIMEOUT_MILLISECONDS", "25000"),
            ("DATABASE_LOCK_TIMEOUT_MILLISECONDS", "3000"),
            ("DATABASE_IDLE_TRANSACTION_TIMEOUT_MILLISECONDS", "12000"),
        ]);
        let config = AppConfig::from_lookup(|name| configured.get(name).map(ToString::to_string))
            .expect("bounded database controls should parse");
        assert!(!config.database.migrate_on_start);
        assert_eq!(config.database.max_connections, 7);
        assert_eq!(config.database.acquire_timeout, Duration::from_secs(4));
        assert_eq!(config.database.statement_timeout, Duration::from_secs(25));
        assert_eq!(config.database.lock_timeout, Duration::from_secs(3));
        assert_eq!(
            config.database.idle_transaction_timeout,
            Duration::from_secs(12)
        );

        for (name, value) in [
            ("DATABASE_MAX_CONNECTIONS", "0"),
            ("DATABASE_MAX_CONNECTIONS", "51"),
            ("DATABASE_ACQUIRE_TIMEOUT_MILLISECONDS", "0"),
            ("DATABASE_ACQUIRE_TIMEOUT_MILLISECONDS", "60001"),
            ("DATABASE_STATEMENT_TIMEOUT_MILLISECONDS", "300001"),
            ("DATABASE_LOCK_TIMEOUT_MILLISECONDS", "60001"),
            ("DATABASE_IDLE_TRANSACTION_TIMEOUT_MILLISECONDS", "300001"),
            ("DATABASE_MIGRATE_ON_START", "yes"),
        ] {
            let values = HashMap::from([(name, value)]);
            let error = AppConfig::from_lookup(|key| values.get(key).map(ToString::to_string))
                .expect_err("out-of-range database control must fail closed");
            assert!(error.to_string().contains(name), "{name} must be named");
        }
    }

    #[test]
    fn private_inspiration_deployment_gate_is_explicit_and_strict() {
        let enabled = HashMap::from([("INSPIRATION_ENABLED", "true")]);
        let config = AppConfig::from_lookup(|name| enabled.get(name).map(ToString::to_string))
            .expect("an explicit true value should enable source loading");
        assert!(config.inspiration_enabled);

        for invalid in ["1", "yes", "TRUE", "on"] {
            let values = HashMap::from([("INSPIRATION_ENABLED", invalid)]);
            assert!(
                AppConfig::from_lookup(|name| values.get(name).map(ToString::to_string)).is_err(),
                "{invalid} must not be interpreted as consent enablement"
            );
        }
    }

    #[test]
    fn content_root_moves_but_default_theme_is_allowlisted() {
        let values = HashMap::from([
            ("CONTENT_PACK_ROOT", "/opt/manchester-arcana/content/packs"),
            ("CONTENT_DEFAULT_THEME_PACK_ID", EMBERLINE_THEME_PACK_ID),
        ]);
        let config = AppConfig::from_lookup(|name| values.get(name).map(ToString::to_string))
            .expect("the other bundled theme and a normalized absolute root are valid");
        assert_eq!(
            config.content_packs.root,
            Path::new("/opt/manchester-arcana/content/packs")
        );
        assert_eq!(
            config.content_packs.default_theme_pack_id,
            EMBERLINE_THEME_PACK_ID
        );

        for (name, value) in [
            ("CONTENT_DEFAULT_THEME_PACK_ID", "dev.example.unreviewed"),
            ("CONTENT_PACK_ROOT", "../outside"),
            ("CONTENT_PACK_ROOT", "content/./packs"),
            ("EVENT_PROMPT_DIR", "prompts/../private"),
            ("IMAGE_ARTIFACT_ROOT", "public/generated-images"),
            ("IMAGE_ARTIFACT_ROOT", "target/site/images"),
        ] {
            let values = HashMap::from([(name, value)]);
            assert!(
                AppConfig::from_lookup(|key| values.get(key).map(ToString::to_string)).is_err(),
                "{name}={value} must fail closed"
            );
        }
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
            (
                "DATABASE_URL",
                "postgresql://db-user:database-super-secret@db.example.test/game",
            ),
            ("TEXT_LLM_BACKEND", "openai-compatible"),
            ("TEXT_LLM_BASE_URL", "https://text.example.test/v1/"),
            ("TEXT_LLM_API_KEY", "text-super-secret"),
            ("TEXT_LLM_MODEL", "narrator"),
            ("TEXT_LLM_ESTIMATED_REQUEST_COST_MICROUSD", "250"),
            ("IMAGE_LLM_BACKEND", "openai"),
            ("IMAGE_LLM_BASE_URL", "https://images.example.test/api/"),
            ("IMAGE_LLM_API_KEY", "image-super-secret"),
            ("IMAGE_LLM_MODEL", "illustrator"),
            ("IMAGE_LLM_ESTIMATED_REQUEST_COST_MICROUSD", "500"),
        ]);

        let config = AppConfig::from_lookup(|name| values.get(name).map(ToString::to_string))
            .expect("profiles should parse");
        let debug = format!("{config:?}");

        assert_eq!(config.text_llm.model.as_deref(), Some("narrator"));
        assert_eq!(config.image_llm.model.as_deref(), Some("illustrator"));
        assert!(!debug.contains("database-super-secret"));
        assert!(!debug.contains("text-super-secret"));
        assert!(!debug.contains("image-super-secret"));
        assert!(debug.contains("[REDACTED]"));

        let fingerprints = config.generation_config_fingerprints();
        let encoded = serde_json::to_string(&fingerprints).unwrap();
        assert!(!encoded.contains("database-super-secret"));
        assert!(!encoded.contains("text-super-secret"));
        assert!(!encoded.contains("image-super-secret"));
        assert_ne!(fingerprints.text, fingerprints.image);
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
        assert!(parse_base_url("IMAGE_LLM_BASE_URL", "https://169.254.169.254/v1").is_err());
    }

    #[test]
    fn paid_capable_image_provider_requires_a_nonzero_cost_estimate() {
        let values = HashMap::from([
            ("IMAGE_LLM_BACKEND", "openai-compatible"),
            ("IMAGE_LLM_BASE_URL", "https://images.example.test/v1/"),
            ("IMAGE_LLM_MODEL", "illustrator"),
            ("IMAGE_LLM_ESTIMATED_REQUEST_COST_MICROUSD", "0"),
        ]);
        assert!(AppConfig::from_lookup(|name| values.get(name).map(ToString::to_string)).is_err());
    }

    #[test]
    fn generation_governance_limits_are_explicit_bounded_and_fingerprinted() {
        let values = HashMap::from([
            ("GENERATION_CAMPAIGN_REQUEST_BUDGET", "20"),
            ("GENERATION_TURN_REQUEST_BUDGET", "3"),
            ("GENERATION_CAMPAIGN_TOKEN_BUDGET", "200000"),
            ("GENERATION_TURN_TOKEN_BUDGET", "20000"),
            ("GENERATION_CAMPAIGN_LATENCY_BUDGET_MILLISECONDS", "90000"),
            ("GENERATION_TURN_LATENCY_BUDGET_MILLISECONDS", "10000"),
            ("GENERATION_CAMPAIGN_COST_BUDGET_MICROUSD", "5000"),
            ("GENERATION_TURN_COST_BUDGET_MICROUSD", "500"),
            ("GENERATION_MAX_CAMPAIGN_CONCURRENCY", "1"),
            ("GENERATION_WORKER_BATCH_SIZE", "7"),
        ]);
        let configured =
            AppConfig::from_lookup(|name| values.get(name).map(ToString::to_string)).unwrap();
        assert_eq!(configured.generation_governance.campaign.requests, 20);
        assert_eq!(configured.generation_governance.turn.tokens, 20_000);
        assert_eq!(configured.generation_governance.worker_batch_size, 7);

        let defaults = AppConfig::from_lookup(|_| None).unwrap();
        assert_ne!(
            configured.generation_governance.non_secret_fingerprint(),
            defaults.generation_governance.non_secret_fingerprint()
        );

        let invalid = HashMap::from([
            ("GENERATION_CAMPAIGN_REQUEST_BUDGET", "2"),
            ("GENERATION_TURN_REQUEST_BUDGET", "3"),
        ]);
        assert!(matches!(
            AppConfig::from_lookup(|name| invalid.get(name).map(ToString::to_string)),
            Err(ConfigError::InvalidValue {
                name: "GENERATION_TURN_REQUEST_BUDGET",
                ..
            })
        ));
    }

    #[test]
    fn paid_provider_profile_requires_an_operator_cost_estimate() {
        let values = HashMap::from([
            ("TEXT_LLM_BACKEND", "openai-compatible"),
            ("TEXT_LLM_BASE_URL", "https://text.example.test/v1"),
            ("TEXT_LLM_MODEL", "bounded-model"),
        ]);
        assert!(matches!(
            AppConfig::from_lookup(|name| values.get(name).map(ToString::to_string)),
            Err(ConfigError::MissingProfileValue {
                name: "TEXT_LLM_ESTIMATED_REQUEST_COST_MICROUSD",
                ..
            })
        ));
    }
}
