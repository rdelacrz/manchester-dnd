use std::{fmt, sync::Arc, time::Duration};

use deadpool_redis::{Config as PoolConfig, Pool, Runtime};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{config::DragonflyConfig, error::CacheError};

const SESSION_KEY_PREFIX: &str = "mdnd:sess:v1:";
const THROTTLE_KEY_PREFIX: &str = "mdnd:throttle:v1:";
const CAMPAIGN_CHANNEL_PREFIX: &str = "mdnd:campaign:v1:";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheHealth {
    Disabled,
    Healthy,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionCacheEntry {
    pub account_id: String,
    pub session_id: String,
    pub role: String,
    pub csrf_digest: String,
    pub idle_expires_at_millis: i64,
    pub absolute_expires_at_millis: i64,
    pub password_role_version: u32,
    pub last_persisted_at_millis: i64,
}

/// Opaque post-commit notification. Subscribers must re-read MongoDB; the
/// event deliberately carries no mutable domain payload or PII.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CampaignDomainEvent {
    pub campaign_digest: String,
    pub event_kind: String,
    pub revision: u64,
    pub emitted_at_millis: i64,
}

#[derive(Clone)]
pub struct CacheService {
    inner: Arc<CacheInner>,
}

#[allow(dead_code, clippy::large_enum_variant)]
enum CacheInner {
    Disabled,
    Enabled {
        pool: Pool,
        client: redis::Client,
        command_timeout: Duration,
    },
}

impl fmt::Debug for CacheService {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CacheService")
            .field("enabled", &self.is_enabled())
            .finish_non_exhaustive()
    }
}

impl CacheService {
    pub fn from_config(config: &DragonflyConfig) -> Result<Self, CacheError> {
        if !config.enabled {
            return Ok(Self::disabled());
        }
        let mut pool_config = PoolConfig::from_url(config.url.expose_secret());
        pool_config.pool = Some(deadpool_redis::PoolConfig::new(config.pool_size));
        let pool = pool_config
            .create_pool(Some(Runtime::Tokio1))
            .map_err(|_| CacheError::InvalidPoolConfiguration)?;
        let client =
            redis::Client::open(config.url.expose_secret()).map_err(CacheError::Command)?;
        Ok(Self {
            inner: Arc::new(CacheInner::Enabled {
                pool,
                client,
                command_timeout: config.command_timeout,
            }),
        })
    }

    pub fn disabled() -> Self {
        Self {
            inner: Arc::new(CacheInner::Disabled),
        }
    }

    pub fn is_enabled(&self) -> bool {
        matches!(self.inner.as_ref(), CacheInner::Enabled { .. })
    }

    pub async fn health_check(&self) -> CacheHealth {
        let CacheInner::Enabled {
            pool,
            command_timeout,
            ..
        } = self.inner.as_ref()
        else {
            return CacheHealth::Disabled;
        };
        let mut command = redis::cmd("PING");
        match run_command::<String>(pool, *command_timeout, &mut command).await {
            Ok(response) if response == "PONG" => CacheHealth::Healthy,
            Ok(_) | Err(_) => CacheHealth::Unavailable,
        }
    }

    /// Cache read failures deliberately become misses. Authoritative callers
    /// must fall through to MongoDB.
    pub async fn get(&self, key: &str) -> Option<Vec<u8>> {
        let CacheInner::Enabled {
            pool,
            command_timeout,
            ..
        } = self.inner.as_ref()
        else {
            return None;
        };
        let mut command = redis::cmd("GET");
        command.arg(key);
        run_command::<Option<Vec<u8>>>(pool, *command_timeout, &mut command)
            .await
            .ok()
            .flatten()
    }

    pub async fn set_ex(&self, key: &str, value: &[u8], ttl: Duration) -> Result<(), CacheError> {
        let Some((pool, timeout)) = self.enabled_parts() else {
            return Ok(());
        };
        let mut command = redis::cmd("SET");
        command.arg(key).arg(value).arg("EX").arg(ttl.as_secs());
        run_command::<()>(pool, timeout, &mut command).await
    }

    /// Malformed entries and transport errors deliberately become misses.
    pub async fn get_session(&self, bearer_digest: &str) -> Option<SessionCacheEntry> {
        if !valid_prefixed_digest(bearer_digest, "sha256:") {
            return None;
        }
        let key = format!("{SESSION_KEY_PREFIX}{bearer_digest}");
        self.get(&key)
            .await
            .and_then(|value| serde_json::from_slice(&value).ok())
    }

    pub async fn set_session(
        &self,
        bearer_digest: &str,
        entry: &SessionCacheEntry,
        ttl: Duration,
    ) -> Result<(), CacheError> {
        if !valid_prefixed_digest(bearer_digest, "sha256:") {
            return Err(CacheError::InvalidKey);
        }
        if ttl.is_zero() {
            return Ok(());
        }
        let value = serde_json::to_vec(entry).map_err(CacheError::Serialization)?;
        self.set_ex(&format!("{SESSION_KEY_PREFIX}{bearer_digest}"), &value, ttl)
            .await
    }

    pub async fn del_session(&self, bearer_digest: &str) -> Result<u64, CacheError> {
        if !valid_prefixed_digest(bearer_digest, "sha256:") {
            return Err(CacheError::InvalidKey);
        }
        self.del(&format!("{SESSION_KEY_PREFIX}{bearer_digest}"))
            .await
    }

    pub async fn del(&self, key: &str) -> Result<u64, CacheError> {
        let Some((pool, timeout)) = self.enabled_parts() else {
            return Ok(0);
        };
        let mut command = redis::cmd("DEL");
        command.arg(key);
        run_command(pool, timeout, &mut command).await
    }

    /// Disabled cache returns `None`; policy layer must use authoritative
    /// MongoDB throttle state instead.
    pub async fn incr(&self, key: &str) -> Result<Option<i64>, CacheError> {
        let Some((pool, timeout)) = self.enabled_parts() else {
            return Ok(None);
        };
        let mut command = redis::cmd("INCR");
        command.arg(key);
        run_command(pool, timeout, &mut command).await.map(Some)
    }

    /// Uses a Redis transaction so every increment has a bounded TTL even if a
    /// concurrent request creates the bucket.
    pub async fn increment_throttle(
        &self,
        action_kind: &str,
        key_digest: &str,
        ttl: Duration,
    ) -> Result<Option<i64>, CacheError> {
        if !matches!(action_kind, "login" | "signup")
            || !valid_prefixed_digest(key_digest, "hmac-sha256:")
        {
            return Err(CacheError::InvalidKey);
        }
        let Some((pool, timeout)) = self.enabled_parts() else {
            return Ok(None);
        };
        let key = format!("{THROTTLE_KEY_PREFIX}{action_kind}:{key_digest}");
        let mut pipeline = redis::pipe();
        pipeline
            .atomic()
            .cmd("INCR")
            .arg(&key)
            .cmd("EXPIRE")
            .arg(&key)
            .arg(ttl.as_secs().max(1));
        let (count, _): (i64, bool) = run_pipeline(pool, timeout, &mut pipeline).await?;
        Ok(Some(count))
    }

    pub async fn expire(&self, key: &str, ttl: Duration) -> Result<bool, CacheError> {
        let Some((pool, timeout)) = self.enabled_parts() else {
            return Ok(false);
        };
        let mut command = redis::cmd("EXPIRE");
        command.arg(key).arg(ttl.as_secs());
        run_command(pool, timeout, &mut command).await
    }

    pub async fn publish(&self, channel: &str, payload: &[u8]) -> Result<u64, CacheError> {
        let Some((pool, timeout)) = self.enabled_parts() else {
            return Ok(0);
        };
        let mut command = redis::cmd("PUBLISH");
        command.arg(channel).arg(payload);
        run_command(pool, timeout, &mut command).await
    }

    /// Call only after the corresponding MongoDB commit succeeds. Publishing
    /// is best effort and carries no authoritative mutable payload.
    pub async fn publish_campaign_event(
        &self,
        campaign_id: &str,
        event_kind: &str,
        revision: u64,
    ) -> Result<u64, CacheError> {
        let Some((channel, campaign_digest)) = campaign_channel(campaign_id) else {
            return Err(CacheError::InvalidKey);
        };
        if event_kind.is_empty()
            || event_kind.len() > 64
            || !event_kind.bytes().all(|byte| {
                byte.is_ascii_lowercase()
                    || byte.is_ascii_digit()
                    || matches!(byte, b'_' | b'-' | b'.')
            })
        {
            return Err(CacheError::InvalidKey);
        }
        let payload = serde_json::to_vec(&CampaignDomainEvent {
            campaign_digest,
            event_kind: event_kind.to_owned(),
            revision,
            emitted_at_millis: mongodb::bson::DateTime::now().timestamp_millis(),
        })
        .map_err(CacheError::Serialization)?;
        self.publish(&channel, &payload).await
    }

    /// Returns `None` when Dragonfly is disabled. A subscriber should revert
    /// to bounded MongoDB polling if this connection cannot be established.
    pub async fn subscribe_campaign_events(
        &self,
        campaign_id: &str,
    ) -> Result<Option<redis::aio::PubSub>, CacheError> {
        let Some((channel, _)) = campaign_channel(campaign_id) else {
            return Err(CacheError::InvalidKey);
        };
        let CacheInner::Enabled {
            client,
            command_timeout,
            ..
        } = self.inner.as_ref()
        else {
            return Ok(None);
        };
        let mut subscription = tokio::time::timeout(*command_timeout, client.get_async_pubsub())
            .await
            .map_err(|_| CacheError::Timeout)?
            .map_err(CacheError::Command)?;
        tokio::time::timeout(*command_timeout, subscription.subscribe(channel))
            .await
            .map_err(|_| CacheError::Timeout)?
            .map_err(CacheError::Command)?;
        Ok(Some(subscription))
    }

    fn enabled_parts(&self) -> Option<(&Pool, Duration)> {
        match self.inner.as_ref() {
            CacheInner::Disabled => None,
            CacheInner::Enabled {
                pool,
                command_timeout,
                ..
            } => Some((pool, *command_timeout)),
        }
    }
}

fn campaign_channel(campaign_id: &str) -> Option<(String, String)> {
    if !(1..=128).contains(&campaign_id.len())
        || !campaign_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b':' | b'.'))
    {
        return None;
    }
    let digest = format!("sha256:{:x}", Sha256::digest(campaign_id.as_bytes()));
    Some((format!("{CAMPAIGN_CHANNEL_PREFIX}{digest}"), digest))
}

fn valid_prefixed_digest(value: &str, prefix: &str) -> bool {
    value.len() == prefix.len() + 64
        && value.starts_with(prefix)
        && value[prefix.len()..]
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

async fn run_command<T>(
    pool: &Pool,
    timeout: Duration,
    command: &mut redis::Cmd,
) -> Result<T, CacheError>
where
    T: redis::FromRedisValue,
{
    let mut connection = tokio::time::timeout(timeout, pool.get())
        .await
        .map_err(|_| CacheError::Timeout)?
        .map_err(CacheError::Pool)?;
    tokio::time::timeout(timeout, command.query_async(&mut connection))
        .await
        .map_err(|_| CacheError::Timeout)?
        .map_err(CacheError::Command)
}

async fn run_pipeline<T>(
    pool: &Pool,
    timeout: Duration,
    pipeline: &mut redis::Pipeline,
) -> Result<T, CacheError>
where
    T: redis::FromRedisValue,
{
    let mut connection = tokio::time::timeout(timeout, pool.get())
        .await
        .map_err(|_| CacheError::Timeout)?
        .map_err(CacheError::Pool)?;
    tokio::time::timeout(timeout, pipeline.query_async(&mut connection))
        .await
        .map_err(|_| CacheError::Timeout)?
        .map_err(CacheError::Command)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::SecretString;

    #[tokio::test]
    async fn disabled_cache_is_an_explicit_miss_and_noop_transport() {
        let cache = CacheService::disabled();
        assert_eq!(cache.health_check().await, CacheHealth::Disabled);
        assert_eq!(cache.get("session:test").await, None);
        assert_eq!(cache.incr("throttle:test").await.unwrap(), None);
        assert_eq!(cache.del("session:test").await.unwrap(), 0);
        assert!(
            !cache
                .expire("session:test", Duration::from_secs(1))
                .await
                .unwrap()
        );
        assert_eq!(cache.publish("campaign:test", b"event").await.unwrap(), 0);
        cache
            .set_ex("session:test", b"value", Duration::from_secs(1))
            .await
            .unwrap();
        assert_eq!(cache.get_session("raw-bearer").await, None);
        assert!(matches!(
            cache
                .set_session(
                    "raw-bearer",
                    &SessionCacheEntry {
                        account_id: "account:test".to_owned(),
                        session_id: "session:test".to_owned(),
                        role: "user".to_owned(),
                        csrf_digest: format!("sha256:{}", "0".repeat(64)),
                        idle_expires_at_millis: 1,
                        absolute_expires_at_millis: 1,
                        password_role_version: 1,
                        last_persisted_at_millis: 1,
                    },
                    Duration::from_secs(1),
                )
                .await,
            Err(CacheError::InvalidKey)
        ));
    }

    #[tokio::test]
    async fn enabled_connection_failure_is_health_unavailable_and_read_miss() {
        let config = DragonflyConfig {
            enabled: true,
            url: SecretString::new("redis://default:secret@127.0.0.1:1/0"),
            pool_size: 1,
            command_timeout: Duration::from_millis(25),
        };
        let cache = CacheService::from_config(&config).unwrap();
        assert_eq!(cache.health_check().await, CacheHealth::Unavailable);
        assert_eq!(cache.get("missing").await, None);
    }
}
