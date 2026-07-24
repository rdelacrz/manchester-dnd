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

const DEFAULT_MONGODB_URI: &str = "mongodb://manchester_app:***@127.0.0.1:27017/?authSource=admin&replicaSet=rs0&directConnection=true";
const DEFAULT_MONGODB_DATABASE: &str = "manchester_dnd";
const DEFAULT_MONGODB_MAX_POOL_SIZE: u32 = 10;
const DEFAULT_MONGODB_MIN_POOL_SIZE: u32 = 0;
const DEFAULT_MONGODB_CONNECT_TIMEOUT_MILLISECONDS: u64 = 5_000;
const DEFAULT_MONGODB_SERVER_SELECTION_TIMEOUT_MILLISECONDS: u64 = 5_000;
const DEFAULT_MONGODB_OPERATION_TIMEOUT_MILLISECONDS: u64 = 30_000;
const DEFAULT_MONGODB_TRANSACTION_TIMEOUT_MILLISECONDS: u64 = 10_000;
const DEFAULT_MONGODB_TRANSACTION_MAX_RETRIES: u32 = 3;
const DEFAULT_DRAGONFLY_URL: &str = "redis://default:dev-cache-password@127.0.0.1:6379/0";
const DEFAULT_DRAGONFLY_POOL_SIZE: usize = 8;
const DEFAULT_DRAGONFLY_TIMEOUT_MILLISECONDS: u64 = 2_000;
const DEFAULT_AUTH_EMAIL_ENCRYPTION_KEY: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";
const DEFAULT_AUTH_EMAIL_ENCRYPTION_KEY_ID: &str = "email-key:dev-only-v1";
const DEFAULT_AUTH_EMAIL_LOOKUP_HMAC_KEY: &str =
    "dev-only-email-lookup-hmac-key-change-before-hosting";
const DEFAULT_AUTH_THROTTLE_HMAC_KEY: &str =
    "dev-only-auth-throttle-hmac-key-change-before-hosting";
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
pub enum MongoSchemaPolicy {
    ApplyAndVerify,
    VerifyOnly,
}

#[derive(Debug, Clone)]
pub struct MongoConfig {
    pub uri: SecretString,
    pub database: String,
    pub max_pool_size: u32,
    pub min_pool_size: u32,
    pub connect_timeout: Duration,
    pub server_selection_timeout: Duration,
    /// Application-level timeout wrapped around MongoDB operations. The Rust
    /// driver intentionally does not support the legacy `socketTimeoutMS`
    /// option, so callers use this explicit deadline instead.
    pub operation_timeout: Duration,
    pub transaction_timeout: Duration,
    pub transaction_max_retries: u32,
    pub schema_policy: MongoSchemaPolicy,
}

#[derive(Debug, Clone)]
pub struct PersistenceConfig {
    pub mongodb: MongoConfig,
}

#[derive(Debug, Clone)]
pub struct DragonflyConfig {
    pub enabled: bool,
    pub url: SecretString,
    pub pool_size: usize,
    pub command_timeout: Duration,
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
    pub email_encryption_key: SecretString,
    pub email_encryption_key_id: String,
    pub email_lookup_hmac_key: SecretString,
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
            throttle_hmac_key: SecretString::new(DEFAULT_AUTH_THROTTLE_HMAC_KEY),
            email_encryption_key: SecretString::new(DEFAULT_AUTH_EMAIL_ENCRYPTION_KEY),
            email_encryption_key_id: DEFAULT_AUTH_EMAIL_ENCRYPTION_KEY_ID.to_owned(),
            email_lookup_hmac_key: SecretString::new(DEFAULT_AUTH_EMAIL_LOOKUP_HMAC_KEY),
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
    pub persistence: PersistenceConfig,
    pub dragonfly: DragonflyConfig,
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
        let mongodb_uri = get("MONGODB_URI")
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_MONGODB_URI.to_owned());
        validate_mongodb_uri(&mongodb_uri, access_mode)?;
        let mongodb_database = get("MONGODB_DATABASE")
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_MONGODB_DATABASE.to_owned());
        validate_mongodb_database_name(&mongodb_database)?;
        let mongodb_max_pool_size = parse_generation_limit(
            &mut get,
            "MONGODB_MAX_POOL_SIZE",
            DEFAULT_MONGODB_MAX_POOL_SIZE,
        )?;
        let mongodb_min_pool_size = parse_generation_limit(
            &mut get,
            "MONGODB_MIN_POOL_SIZE",
            DEFAULT_MONGODB_MIN_POOL_SIZE,
        )?;
        if !(1..=100).contains(&mongodb_max_pool_size) {
            return Err(ConfigError::InvalidValue {
                name: "MONGODB_MAX_POOL_SIZE",
                reason: "must be between 1 and 100".to_owned(),
            });
        }
        if mongodb_min_pool_size > mongodb_max_pool_size {
            return Err(ConfigError::InvalidValue {
                name: "MONGODB_MIN_POOL_SIZE",
                reason: "must not exceed MONGODB_MAX_POOL_SIZE".to_owned(),
            });
        }
        let mongodb_connect_timeout = Duration::from_millis(parse_generation_limit(
            &mut get,
            "MONGODB_CONNECT_TIMEOUT_MS",
            DEFAULT_MONGODB_CONNECT_TIMEOUT_MILLISECONDS,
        )?);
        let mongodb_server_selection_timeout = Duration::from_millis(parse_generation_limit(
            &mut get,
            "MONGODB_SERVER_SELECTION_TIMEOUT_MS",
            DEFAULT_MONGODB_SERVER_SELECTION_TIMEOUT_MILLISECONDS,
        )?);
        let mongodb_operation_timeout = Duration::from_millis(parse_generation_limit(
            &mut get,
            "MONGODB_OPERATION_TIMEOUT_MS",
            DEFAULT_MONGODB_OPERATION_TIMEOUT_MILLISECONDS,
        )?);
        let mongodb_transaction_timeout = Duration::from_millis(parse_generation_limit(
            &mut get,
            "MONGODB_TRANSACTION_TIMEOUT_MS",
            DEFAULT_MONGODB_TRANSACTION_TIMEOUT_MILLISECONDS,
        )?);
        for (name, value, maximum) in [
            (
                "MONGODB_CONNECT_TIMEOUT_MS",
                mongodb_connect_timeout,
                Duration::from_secs(60),
            ),
            (
                "MONGODB_SERVER_SELECTION_TIMEOUT_MS",
                mongodb_server_selection_timeout,
                Duration::from_secs(60),
            ),
            (
                "MONGODB_OPERATION_TIMEOUT_MS",
                mongodb_operation_timeout,
                Duration::from_secs(300),
            ),
            (
                "MONGODB_TRANSACTION_TIMEOUT_MS",
                mongodb_transaction_timeout,
                Duration::from_secs(60),
            ),
        ] {
            if value.is_zero() || value > maximum {
                return Err(ConfigError::InvalidValue {
                    name,
                    reason: format!("must be between 1 and {} milliseconds", maximum.as_millis()),
                });
            }
        }
        let mongodb_transaction_max_retries = parse_generation_limit(
            &mut get,
            "MONGODB_TRANSACTION_MAX_RETRIES",
            DEFAULT_MONGODB_TRANSACTION_MAX_RETRIES,
        )?;
        if mongodb_transaction_max_retries > 5 {
            return Err(ConfigError::InvalidValue {
                name: "MONGODB_TRANSACTION_MAX_RETRIES",
                reason: "must be between 0 and 5".to_owned(),
            });
        }
        let schema_apply_on_start = parse_optional_bool(
            get("MONGO_SCHEMA_APPLY_ON_START"),
            "MONGO_SCHEMA_APPLY_ON_START",
        )?
        .unwrap_or(access_mode == AccessMode::LocalSingleUser);
        if access_mode == AccessMode::Hosted && schema_apply_on_start {
            return Err(ConfigError::InvalidValue {
                name: "MONGO_SCHEMA_APPLY_ON_START",
                reason: "hosted mode is verify-only; apply schema with mongo-admin".to_owned(),
            });
        }
        let schema_policy = if schema_apply_on_start {
            MongoSchemaPolicy::ApplyAndVerify
        } else {
            MongoSchemaPolicy::VerifyOnly
        };

        let dragonfly_enabled =
            parse_optional_bool(get("DRAGONFLY_ENABLED"), "DRAGONFLY_ENABLED")?.unwrap_or(false);
        let dragonfly_url = get("DRAGONFLY_URL")
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_DRAGONFLY_URL.to_owned());
        validate_dragonfly_url(&dragonfly_url, dragonfly_enabled)?;
        let dragonfly_pool_size =
            parse_generation_limit(&mut get, "DRAGONFLY_POOL_SIZE", DEFAULT_DRAGONFLY_POOL_SIZE)?;
        if !(1..=64).contains(&dragonfly_pool_size) {
            return Err(ConfigError::InvalidValue {
                name: "DRAGONFLY_POOL_SIZE",
                reason: "must be between 1 and 64".to_owned(),
            });
        }
        let dragonfly_timeout = Duration::from_millis(parse_generation_limit(
            &mut get,
            "DRAGONFLY_TIMEOUT_MS",
            DEFAULT_DRAGONFLY_TIMEOUT_MILLISECONDS,
        )?);
        if dragonfly_timeout.is_zero() || dragonfly_timeout > Duration::from_secs(30) {
            return Err(ConfigError::InvalidValue {
                name: "DRAGONFLY_TIMEOUT_MS",
                reason: "must be between 1 and 30000 milliseconds".to_owned(),
            });
        }
        let authentication = authentication_from_lookup(access_mode, &mut get)?;

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
            persistence: PersistenceConfig {
                mongodb: MongoConfig {
                    uri: SecretString::new(mongodb_uri),
                    database: mongodb_database,
                    max_pool_size: mongodb_max_pool_size,
                    min_pool_size: mongodb_min_pool_size,
                    connect_timeout: mongodb_connect_timeout,
                    server_selection_timeout: mongodb_server_selection_timeout,
                    operation_timeout: mongodb_operation_timeout,
                    transaction_timeout: mongodb_transaction_timeout,
                    transaction_max_retries: mongodb_transaction_max_retries,
                    schema_policy,
                },
            },
            dragonfly: DragonflyConfig {
                enabled: dragonfly_enabled,
                url: SecretString::new(dragonfly_url),
                pool_size: dragonfly_pool_size,
                command_timeout: dragonfly_timeout,
            },
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
            authentication,
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

pub fn validate_mongodb_database_name(name: &str) -> Result<(), ConfigError> {
    let valid = (1..=63).contains(&name.len())
        && name
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_alphanumeric())
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        && !matches!(
            name.to_ascii_lowercase().as_str(),
            "admin" | "config" | "local"
        );
    if !valid {
        return Err(ConfigError::InvalidValue {
            name: "MONGODB_DATABASE",
            reason: "must be 1-63 ASCII letters, digits, underscores, or hyphens, start with a letter or digit, and not name a MongoDB system database".to_owned(),
        });
    }
    Ok(())
}

pub fn validate_mongodb_uri(raw: &str, access_mode: AccessMode) -> Result<(), ConfigError> {
    let (scheme, remainder) = if let Some(remainder) = raw.strip_prefix("mongodb://") {
        ("mongodb", remainder)
    } else if let Some(remainder) = raw.strip_prefix("mongodb+srv://") {
        ("mongodb+srv", remainder)
    } else {
        return Err(ConfigError::InvalidValue {
            name: "MONGODB_URI",
            reason: "must use mongodb:// or mongodb+srv://".to_owned(),
        });
    };
    if remainder.is_empty()
        || raw.trim() != raw
        || raw.chars().any(char::is_control)
        || raw.contains('#')
    {
        return Err(ConfigError::InvalidValue {
            name: "MONGODB_URI",
            reason: "must be a normalized MongoDB connection URL without fragments".to_owned(),
        });
    }

    let authority = remainder
        .split(['/', '?'])
        .next()
        .filter(|authority| !authority.is_empty())
        .ok_or_else(|| ConfigError::InvalidValue {
            name: "MONGODB_URI",
            reason: "must include at least one host".to_owned(),
        })?;
    let (credentials, hosts) = authority
        .rsplit_once('@')
        .map_or((None, authority), |(credentials, hosts)| {
            (Some(credentials), hosts)
        });
    if hosts.split(',').any(|host| host.trim().is_empty()) {
        return Err(ConfigError::InvalidValue {
            name: "MONGODB_URI",
            reason: "must include only non-empty hosts".to_owned(),
        });
    }

    let query_value = |wanted: &str| {
        raw.split_once('?').and_then(|(_, query)| {
            query.split('&').find_map(|pair| {
                let (key, value) = pair.split_once('=')?;
                key.eq_ignore_ascii_case(wanted).then_some(value)
            })
        })
    };
    if scheme == "mongodb" && query_value("replicaSet").is_none() {
        return Err(ConfigError::InvalidValue {
            name: "MONGODB_URI",
            reason: "mongodb:// URLs must select a replica set".to_owned(),
        });
    }

    if access_mode == AccessMode::Hosted {
        let has_password = credentials
            .and_then(|credentials| credentials.split_once(':'))
            .is_some_and(|(username, password)| !username.is_empty() && !password.is_empty());
        if !has_password {
            return Err(ConfigError::InvalidValue {
                name: "MONGODB_URI",
                reason: "hosted mode requires authenticated MongoDB credentials".to_owned(),
            });
        }
        let tls_enabled = scheme == "mongodb+srv"
            || query_value("tls").is_some_and(|value| value.eq_ignore_ascii_case("true"))
            || query_value("ssl").is_some_and(|value| value.eq_ignore_ascii_case("true"));
        let insecure_tls = ["tlsInsecure", "tlsAllowInvalidCertificates"]
            .into_iter()
            .any(|key| query_value(key).is_some_and(|value| value.eq_ignore_ascii_case("true")));
        if !tls_enabled || insecure_tls {
            return Err(ConfigError::InvalidValue {
                name: "MONGODB_URI",
                reason: "hosted mode requires certificate-validated TLS".to_owned(),
            });
        }
    }
    Ok(())
}

fn validate_dragonfly_url(raw: &str, enabled: bool) -> Result<(), ConfigError> {
    let url = Url::parse(raw).map_err(|_| ConfigError::InvalidValue {
        name: "DRAGONFLY_URL",
        reason: "must be a valid redis:// or rediss:// URL".to_owned(),
    })?;
    if !matches!(url.scheme(), "redis" | "rediss")
        || url.host().is_none()
        || url.fragment().is_some()
    {
        return Err(ConfigError::InvalidValue {
            name: "DRAGONFLY_URL",
            reason: "must be a valid redis:// or rediss:// URL without a fragment".to_owned(),
        });
    }
    if enabled && url.password().is_none_or(str::is_empty) {
        return Err(ConfigError::InvalidValue {
            name: "DRAGONFLY_URL",
            reason: "enabled DragonflyDB requires password authentication".to_owned(),
        });
    }
    Ok(())
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

fn parse_optional_bool(
    value: Option<String>,
    name: &'static str,
) -> Result<Option<bool>, ConfigError> {
    value
        .filter(|value| !value.trim().is_empty())
        .map(|value| {
            value
                .parse::<bool>()
                .map_err(|_| ConfigError::InvalidValue {
                    name,
                    reason: "must be true or false".to_owned(),
                })
        })
        .transpose()
}

fn authentication_from_lookup(
    access_mode: AccessMode,
    get: &mut impl FnMut(&str) -> Option<String>,
) -> Result<AuthenticationConfig, ConfigError> {
    let defaults = AuthenticationConfig::default();
    let session_idle_lifetime = Duration::from_secs(parse_generation_limit(
        get,
        "AUTH_SESSION_IDLE_LIFETIME_SECONDS",
        defaults.session_idle_lifetime.as_secs(),
    )?);
    let session_absolute_lifetime = Duration::from_secs(parse_generation_limit(
        get,
        "AUTH_SESSION_ABSOLUTE_LIFETIME_SECONDS",
        defaults.session_absolute_lifetime.as_secs(),
    )?);
    if session_idle_lifetime.is_zero()
        || session_absolute_lifetime.is_zero()
        || session_idle_lifetime > session_absolute_lifetime
        || session_absolute_lifetime > Duration::from_secs(60 * 60 * 24 * 365)
    {
        return Err(ConfigError::InvalidValue {
            name: "AUTH_SESSION_*_LIFETIME_SECONDS",
            reason: "idle and absolute lifetimes must be positive, idle must not exceed absolute, and absolute must not exceed one year".to_owned(),
        });
    }
    let max_active_sessions = parse_generation_limit(
        get,
        "AUTH_MAX_ACTIVE_SESSIONS",
        defaults.max_active_sessions,
    )?;
    if !(1..=100).contains(&max_active_sessions) {
        return Err(ConfigError::InvalidValue {
            name: "AUTH_MAX_ACTIVE_SESSIONS",
            reason: "must be between 1 and 100".to_owned(),
        });
    }
    let max_hash_concurrency = parse_generation_limit(
        get,
        "AUTH_MAX_HASH_CONCURRENCY",
        defaults.max_hash_concurrency,
    )?;
    if !(1..=64).contains(&max_hash_concurrency) {
        return Err(ConfigError::InvalidValue {
            name: "AUTH_MAX_HASH_CONCURRENCY",
            reason: "must be between 1 and 64".to_owned(),
        });
    }
    let throttle_window_seconds = parse_generation_limit(
        get,
        "AUTH_THROTTLE_WINDOW_SECONDS",
        defaults.throttle_window_seconds,
    )?;
    let throttle_block_after_attempts = parse_generation_limit(
        get,
        "AUTH_THROTTLE_BLOCK_AFTER_ATTEMPTS",
        defaults.throttle_block_after_attempts,
    )?;
    let throttle_block_seconds = parse_generation_limit(
        get,
        "AUTH_THROTTLE_BLOCK_SECONDS",
        defaults.throttle_block_seconds,
    )?;
    if throttle_window_seconds == 0
        || throttle_window_seconds > 86_400
        || throttle_block_after_attempts == 0
        || throttle_block_after_attempts > 10_000
        || throttle_block_seconds == 0
        || throttle_block_seconds > 86_400
    {
        return Err(ConfigError::InvalidValue {
            name: "AUTH_THROTTLE_*",
            reason: "throttle window, threshold, and block duration are outside supported bounds"
                .to_owned(),
        });
    }

    let required_secret = |name: &'static str,
                           local_default: &'static str,
                           get: &mut dyn FnMut(&str) -> Option<String>|
     -> Result<String, ConfigError> {
        match get(name).filter(|value| !value.is_empty()) {
            Some(value) => Ok(value),
            None if access_mode == AccessMode::LocalSingleUser => Ok(local_default.to_owned()),
            None => Err(ConfigError::InvalidValue {
                name,
                reason: "must be explicitly set in hosted mode".to_owned(),
            }),
        }
    };
    let throttle_hmac_key = required_secret(
        "AUTH_THROTTLE_HMAC_KEY",
        DEFAULT_AUTH_THROTTLE_HMAC_KEY,
        get,
    )?;
    if throttle_hmac_key.len() < 32 {
        return Err(ConfigError::InvalidValue {
            name: "AUTH_THROTTLE_HMAC_KEY",
            reason: "must contain at least 32 bytes".to_owned(),
        });
    }
    let email_encryption_key = required_secret(
        "AUTH_EMAIL_ENCRYPTION_KEY_B64",
        DEFAULT_AUTH_EMAIL_ENCRYPTION_KEY,
        get,
    )?;
    crate::persistence::email_crypto::validate_encryption_key_b64(&email_encryption_key).map_err(
        |_| ConfigError::InvalidValue {
            name: "AUTH_EMAIL_ENCRYPTION_KEY_B64",
            reason: "must be standard base64 encoding of exactly 32 bytes".to_owned(),
        },
    )?;
    let email_lookup_hmac_key = required_secret(
        "AUTH_EMAIL_LOOKUP_HMAC_KEY",
        DEFAULT_AUTH_EMAIL_LOOKUP_HMAC_KEY,
        get,
    )?;
    crate::persistence::email_crypto::validate_lookup_key(email_lookup_hmac_key.as_bytes())
        .map_err(|_| ConfigError::InvalidValue {
            name: "AUTH_EMAIL_LOOKUP_HMAC_KEY",
            reason: "must contain at least 32 bytes".to_owned(),
        })?;
    let email_encryption_key_id =
        match get("AUTH_EMAIL_ENCRYPTION_KEY_ID").filter(|value| !value.is_empty()) {
            Some(value) => value,
            None if access_mode == AccessMode::LocalSingleUser => {
                DEFAULT_AUTH_EMAIL_ENCRYPTION_KEY_ID.to_owned()
            }
            None => {
                return Err(ConfigError::InvalidValue {
                    name: "AUTH_EMAIL_ENCRYPTION_KEY_ID",
                    reason: "must be explicitly set in hosted mode".to_owned(),
                });
            }
        };
    let key_id_valid = (1..=128).contains(&email_encryption_key_id.len())
        && email_encryption_key_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b':' | b'-'));
    if !key_id_valid {
        return Err(ConfigError::InvalidValue {
            name: "AUTH_EMAIL_ENCRYPTION_KEY_ID",
            reason: "must be a bounded opaque key identifier".to_owned(),
        });
    }

    let cookie_secure =
        parse_optional_bool(get("AUTH_COOKIE_SECURE"), "AUTH_COOKIE_SECURE")?.unwrap_or(false);
    let canonical_origin = get("AUTH_CANONICAL_ORIGIN").filter(|value| !value.trim().is_empty());
    if let Some(origin) = canonical_origin.as_deref() {
        let parsed = Url::parse(origin).map_err(|_| ConfigError::InvalidValue {
            name: "AUTH_CANONICAL_ORIGIN",
            reason: "must be an absolute normalized origin URL".to_owned(),
        })?;
        if parsed.scheme() != "https"
            || parsed.host().is_none()
            || !parsed.username().is_empty()
            || parsed.password().is_some()
            || parsed.query().is_some()
            || parsed.fragment().is_some()
            || parsed.path() != "/"
        {
            return Err(ConfigError::InvalidValue {
                name: "AUTH_CANONICAL_ORIGIN",
                reason: "must be an HTTPS origin without credentials, path, query, or fragment"
                    .to_owned(),
            });
        }
    }
    if access_mode == AccessMode::Hosted && (!cookie_secure || canonical_origin.is_none()) {
        return Err(ConfigError::InvalidValue {
            name: "AUTH_COOKIE_SECURE/AUTH_CANONICAL_ORIGIN",
            reason: "hosted mode requires secure cookies and an explicit canonical HTTPS origin"
                .to_owned(),
        });
    }

    let argon2_memory_kib =
        parse_generation_limit(get, "AUTH_ARGON2_MEMORY_KIB", defaults.argon2_memory_kib)?;
    let argon2_iterations =
        parse_generation_limit(get, "AUTH_ARGON2_ITERATIONS", defaults.argon2_iterations)?;
    let argon2_parallelism =
        parse_generation_limit(get, "AUTH_ARGON2_PARALLELISM", defaults.argon2_parallelism)?;
    if !(8_192..=1_048_576).contains(&argon2_memory_kib)
        || !(1..=20).contains(&argon2_iterations)
        || !(1..=32).contains(&argon2_parallelism)
    {
        return Err(ConfigError::InvalidValue {
            name: "AUTH_ARGON2_*",
            reason: "Argon2id parameters are outside supported bounds".to_owned(),
        });
    }

    Ok(AuthenticationConfig {
        session_idle_lifetime,
        session_absolute_lifetime,
        max_active_sessions,
        max_hash_concurrency,
        throttle_window_seconds,
        throttle_block_after_attempts,
        throttle_block_seconds,
        throttle_hmac_key: SecretString::new(throttle_hmac_key),
        email_encryption_key: SecretString::new(email_encryption_key),
        email_encryption_key_id,
        email_lookup_hmac_key: SecretString::new(email_lookup_hmac_key),
        cookie_secure,
        canonical_origin,
        argon2_memory_kib,
        argon2_iterations,
        argon2_parallelism,
    })
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
    fn mongo_pool_timeout_schema_and_database_controls_are_bounded() {
        let configured = HashMap::from([
            ("MONGODB_DATABASE", "contract_test-01"),
            ("MONGODB_MAX_POOL_SIZE", "16"),
            ("MONGODB_MIN_POOL_SIZE", "2"),
            ("MONGODB_CONNECT_TIMEOUT_MS", "4000"),
            ("MONGODB_SERVER_SELECTION_TIMEOUT_MS", "4500"),
            ("MONGODB_OPERATION_TIMEOUT_MS", "25000"),
            ("MONGODB_TRANSACTION_TIMEOUT_MS", "9000"),
            ("MONGODB_TRANSACTION_MAX_RETRIES", "2"),
            ("MONGO_SCHEMA_APPLY_ON_START", "false"),
        ]);
        let config = AppConfig::from_lookup(|name| configured.get(name).map(ToString::to_string))
            .expect("bounded MongoDB controls should parse");
        let mongo = &config.persistence.mongodb;

        assert_eq!(mongo.database, "contract_test-01");
        assert_eq!(mongo.max_pool_size, 16);
        assert_eq!(mongo.min_pool_size, 2);
        assert_eq!(mongo.connect_timeout, Duration::from_secs(4));
        assert_eq!(mongo.server_selection_timeout, Duration::from_millis(4_500));
        assert_eq!(mongo.operation_timeout, Duration::from_secs(25));
        assert_eq!(mongo.transaction_timeout, Duration::from_secs(9));
        assert_eq!(mongo.transaction_max_retries, 2);
        assert_eq!(mongo.schema_policy, MongoSchemaPolicy::VerifyOnly);

        for (name, value) in [
            ("MONGODB_MAX_POOL_SIZE", "0"),
            ("MONGODB_MAX_POOL_SIZE", "101"),
            ("MONGODB_MIN_POOL_SIZE", "11"),
            ("MONGODB_CONNECT_TIMEOUT_MS", "0"),
            ("MONGODB_SERVER_SELECTION_TIMEOUT_MS", "60001"),
            ("MONGODB_OPERATION_TIMEOUT_MS", "300001"),
            ("MONGODB_TRANSACTION_TIMEOUT_MS", "60001"),
            ("MONGODB_TRANSACTION_MAX_RETRIES", "6"),
            ("MONGO_SCHEMA_APPLY_ON_START", "yes"),
        ] {
            let values = HashMap::from([(name, value)]);
            let error = AppConfig::from_lookup(|key| values.get(key).map(ToString::to_string))
                .expect_err("out-of-range MongoDB control must fail closed");
            assert!(error.to_string().contains(name), "{name} must be named");
        }
    }

    #[test]
    fn mongo_database_name_uses_a_narrow_non_system_allowlist() {
        for valid in ["manchester_dnd", "test-01", "A1"] {
            assert!(validate_mongodb_database_name(valid).is_ok(), "{valid}");
        }
        for invalid in [
            "",
            "_hidden",
            "has.dot",
            "has space",
            "admin",
            "CONFIG",
            "local",
            &"a".repeat(64),
        ] {
            assert!(
                validate_mongodb_database_name(invalid).is_err(),
                "{invalid}"
            );
        }
    }

    #[test]
    fn hosted_mongo_requires_auth_replica_set_and_valid_tls() {
        for invalid in [
            "mongodb://db.example.test:27017/?replicaSet=rs0&tls=true",
            "mongodb://user:pass@db.example.test:27017/?replicaSet=rs0",
            "mongodb://user:pass@db.example.test:27017/?tls=true",
            "mongodb://user:pass@db.example.test:27017/?replicaSet=rs0&tls=true&tlsInsecure=true",
        ] {
            assert!(
                validate_mongodb_uri(invalid, AccessMode::Hosted).is_err(),
                "{invalid}"
            );
        }
        assert!(
            validate_mongodb_uri(
                "mongodb://user:pass@db.example.test:27017/?replicaSet=rs0&tls=true",
                AccessMode::Hosted
            )
            .is_ok()
        );
        assert!(
            validate_mongodb_uri(
                "mongodb+srv://user:pass@cluster.example.test/?retryWrites=true",
                AccessMode::Hosted
            )
            .is_ok()
        );
    }

    #[test]
    fn malformed_mongo_urls_and_hosted_missing_configuration_fail_closed() {
        for invalid in [
            "https://db.example.test/game",
            "mongodb:///?replicaSet=rs0",
            "mongodb://user:pass@db.example.test/",
            "mongodb://user:pass@db.example.test/?replicaSet=rs0#fragment",
        ] {
            let values = HashMap::from([("MONGODB_URI", invalid)]);
            assert!(
                AppConfig::from_lookup(|name| values.get(name).map(ToString::to_string)).is_err(),
                "{invalid}"
            );
        }

        let hosted = HashMap::from([("APP_ACCESS_MODE", "hosted")]);
        assert!(
            AppConfig::from_lookup(|name| hosted.get(name).map(ToString::to_string)).is_err(),
            "hosted mode must not inherit the plaintext local MongoDB default"
        );
    }

    #[test]
    fn dragonfly_is_disabled_by_default_and_enabled_urls_require_auth() {
        let defaults = AppConfig::from_lookup(|_| None).unwrap();
        assert!(!defaults.dragonfly.enabled);
        assert_eq!(defaults.dragonfly.pool_size, 8);
        assert_eq!(defaults.dragonfly.command_timeout, Duration::from_secs(2));

        let unauthenticated = HashMap::from([
            ("DRAGONFLY_ENABLED", "true"),
            ("DRAGONFLY_URL", "redis://127.0.0.1:6379/0"),
        ]);
        assert!(
            AppConfig::from_lookup(|name| unauthenticated.get(name).map(ToString::to_string))
                .is_err()
        );
        let authenticated = HashMap::from([
            ("DRAGONFLY_ENABLED", "true"),
            ("DRAGONFLY_URL", "redis://:cache-secret@127.0.0.1:6379/0"),
            ("DRAGONFLY_POOL_SIZE", "4"),
            ("DRAGONFLY_TIMEOUT_MS", "750"),
        ]);
        let config =
            AppConfig::from_lookup(|name| authenticated.get(name).map(ToString::to_string))
                .unwrap();
        assert!(config.dragonfly.enabled);
        assert_eq!(config.dragonfly.pool_size, 4);
        assert_eq!(config.dragonfly.command_timeout, Duration::from_millis(750));
        assert!(!format!("{config:?}").contains("cache-secret"));

        for (name, value) in [
            ("DRAGONFLY_POOL_SIZE", "0"),
            ("DRAGONFLY_POOL_SIZE", "65"),
            ("DRAGONFLY_TIMEOUT_MS", "0"),
            ("DRAGONFLY_TIMEOUT_MS", "30001"),
            ("DRAGONFLY_ENABLED", "yes"),
        ] {
            let values = HashMap::from([(name, value)]);
            let error = AppConfig::from_lookup(|key| values.get(key).map(ToString::to_string))
                .expect_err("out-of-range Dragonfly control must fail closed");
            assert!(error.to_string().contains(name));
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
        let values = HashMap::from([
            ("APP_ACCESS_MODE", "hosted"),
            (
                "MONGODB_URI",
                "mongodb://user:pass@db.example.test:27017/?replicaSet=rs0&tls=true",
            ),
            ("AUTH_COOKIE_SECURE", "true"),
            ("AUTH_CANONICAL_ORIGIN", "https://game.example.test"),
            (
                "AUTH_THROTTLE_HMAC_KEY",
                "hosted-throttle-hmac-key-with-at-least-32-bytes",
            ),
            (
                "AUTH_EMAIL_ENCRYPTION_KEY_B64",
                "AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE=",
            ),
            ("AUTH_EMAIL_ENCRYPTION_KEY_ID", "email-key:hosted-v1"),
            (
                "AUTH_EMAIL_LOOKUP_HMAC_KEY",
                "hosted-email-lookup-key-with-at-least-32-bytes",
            ),
        ]);
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
    fn authentication_email_keys_are_separate_bounded_and_redacted() {
        let values = HashMap::from([
            (
                "AUTH_EMAIL_ENCRYPTION_KEY_B64",
                "AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE=",
            ),
            ("AUTH_EMAIL_ENCRYPTION_KEY_ID", "email-key:test-v1"),
            (
                "AUTH_EMAIL_LOOKUP_HMAC_KEY",
                "email-lookup-super-secret-at-least-32-bytes",
            ),
            (
                "AUTH_THROTTLE_HMAC_KEY",
                "throttle-super-secret-at-least-32-bytes",
            ),
        ]);
        let config = AppConfig::from_lookup(|name| values.get(name).map(ToString::to_string))
            .expect("valid independent auth keys should parse");
        let debug = format!("{config:?}");
        assert_eq!(
            config.authentication.email_encryption_key_id,
            "email-key:test-v1"
        );
        assert!(!debug.contains("AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE="));
        assert!(!debug.contains("email-lookup-super-secret"));
        assert!(!debug.contains("throttle-super-secret"));

        for (name, value) in [
            ("AUTH_EMAIL_ENCRYPTION_KEY_B64", "not-base64"),
            ("AUTH_EMAIL_ENCRYPTION_KEY_B64", "YWJj"),
            ("AUTH_EMAIL_LOOKUP_HMAC_KEY", "too-short"),
            ("AUTH_THROTTLE_HMAC_KEY", "too-short"),
            ("AUTH_EMAIL_ENCRYPTION_KEY_ID", "key id with spaces"),
        ] {
            let invalid = HashMap::from([(name, value)]);
            let error = AppConfig::from_lookup(|key| invalid.get(key).map(ToString::to_string))
                .expect_err("invalid auth key material must fail closed");
            assert!(error.to_string().contains(name));
        }
    }

    #[test]
    fn hosted_authentication_requires_explicit_crypto_and_cookie_values() {
        let values = HashMap::from([
            ("APP_ACCESS_MODE", "hosted"),
            (
                "MONGODB_URI",
                "mongodb://user:pass@db.example.test:27017/?replicaSet=rs0&tls=true",
            ),
            ("AUTH_COOKIE_SECURE", "true"),
            ("AUTH_CANONICAL_ORIGIN", "https://game.example.test"),
            (
                "AUTH_THROTTLE_HMAC_KEY",
                "hosted-throttle-hmac-key-with-at-least-32-bytes",
            ),
            (
                "AUTH_EMAIL_ENCRYPTION_KEY_B64",
                "AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE=",
            ),
            ("AUTH_EMAIL_ENCRYPTION_KEY_ID", "email-key:hosted-v1"),
            (
                "AUTH_EMAIL_LOOKUP_HMAC_KEY",
                "hosted-email-lookup-key-with-at-least-32-bytes",
            ),
        ]);
        for missing in [
            "AUTH_COOKIE_SECURE",
            "AUTH_CANONICAL_ORIGIN",
            "AUTH_THROTTLE_HMAC_KEY",
            "AUTH_EMAIL_ENCRYPTION_KEY_B64",
            "AUTH_EMAIL_ENCRYPTION_KEY_ID",
            "AUTH_EMAIL_LOOKUP_HMAC_KEY",
        ] {
            let error = AppConfig::from_lookup(|name| {
                if name == missing {
                    None
                } else {
                    values.get(name).map(ToString::to_string)
                }
            })
            .expect_err("hosted authentication value must be explicit");
            assert!(
                error.to_string().contains(missing)
                    || error
                        .to_string()
                        .contains("AUTH_COOKIE_SECURE/AUTH_CANONICAL_ORIGIN"),
                "{missing}: {error}"
            );
        }
    }

    #[test]
    fn profiles_are_independently_configurable_and_secrets_are_redacted() {
        let values = HashMap::from([
            (
                "MONGODB_URI",
                "mongodb://mongo-user:mongo-super-secret@127.0.0.1:27017/?replicaSet=rs0",
            ),
            (
                "DRAGONFLY_URL",
                "redis://:dragonfly-super-secret@127.0.0.1:6379/0",
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
        assert!(!debug.contains("mongo-super-secret"));
        assert!(!debug.contains("dragonfly-super-secret"));
        assert!(!debug.contains("text-super-secret"));
        assert!(!debug.contains("image-super-secret"));
        assert!(debug.contains("[REDACTED]"));

        let fingerprints = config.generation_config_fingerprints();
        let encoded = serde_json::to_string(&fingerprints).unwrap();
        assert!(!encoded.contains("mongo-super-secret"));
        assert!(!encoded.contains("dragonfly-super-secret"));
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
