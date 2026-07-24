//! Account authentication and opaque server-side session service.
//!
//! MongoDB is authoritative. DragonflyDB is a bounded read-through session
//! cache and throttle fast path; every cache error falls back to MongoDB.

use std::{fmt, sync::Arc, time::Duration};

use argon2::{
    Algorithm, Argon2, Params, Version,
    password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use mongodb::bson::DateTime;
use rand::TryRngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::Semaphore;
use uuid::Uuid;
use zeroize::Zeroize;

use crate::{
    cache::{CacheService, SessionCacheEntry},
    config::AuthenticationConfig,
    error::{AuthenticationError, MongoFailureKind, PersistenceError},
    persistence::{
        EmailCrypto, MongoAccountRepository,
        auth::{
            CompleteSignupWrite, MongoLoginAccount, MongoSessionRecord, NewMongoAccount,
            NewMongoAccountSession, NewSignupAccessToken, NewSignupSession, add_duration,
            purge_after,
        },
    },
};

pub const LOCAL_ACCOUNT_ID: &str = "account:local";
pub const SHA256_DIGEST_PREFIX: &str = "sha256:";
const SIGNUP_SESSION_LIFETIME: Duration = Duration::from_secs(24 * 60 * 60);
const MAX_ACCESS_TOKEN_LIFETIME: Duration = Duration::from_secs(90 * 24 * 60 * 60);
const SESSION_CACHE_MAX_TTL: Duration = Duration::from_secs(300);

#[derive(Clone, PartialEq, Eq)]
pub struct PasswordPhc(String);

impl PasswordPhc {
    pub fn parse(value: impl Into<String>) -> Result<Self, AuthenticationInputError> {
        let value = value.into();
        if !(32..=1024).contains(&value.len()) || !value.starts_with("$argon2id$") {
            return Err(AuthenticationInputError::InvalidPasswordHash);
        }
        Ok(Self(value))
    }

    pub(crate) fn expose_secret(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for PasswordPhc {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("PasswordPhc([REDACTED])")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccountPrincipal {
    pub account_id: String,
    pub session_id: String,
}

/// Transitional SQL shape retained only so untouched SQL repository modules
/// compile during the MongoDB vertical migration.
#[derive(Clone, PartialEq, Eq)]
pub struct NewAccount {
    pub id: String,
    pub normalized_email: String,
    pub display_name: String,
    pub password_phc: PasswordPhc,
}

impl fmt::Debug for NewAccount {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NewAccount")
            .field("id", &self.id)
            .field("normalized_email", &"[REDACTED]")
            .field("display_name", &self.display_name)
            .field("password_phc", &self.password_phc)
            .finish()
    }
}

/// Transitional SQL shape retained only for untouched repositories.
#[derive(Clone, PartialEq, Eq)]
pub struct LoginAccount {
    pub id: String,
    pub display_name: String,
    pub password_phc: PasswordPhc,
    pub login_enabled: bool,
    pub created_at: String,
    pub updated_at: String,
}

impl fmt::Debug for LoginAccount {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LoginAccount")
            .field("id", &self.id)
            .field("display_name", &self.display_name)
            .field("password_phc", &self.password_phc)
            .field("login_enabled", &self.login_enabled)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountSummary {
    pub id: String,
    pub display_name: String,
    pub login_enabled: bool,
    pub created_at: String,
    pub updated_at: String,
}

/// Transitional SQL shape retained only for untouched repositories.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewAccountSession {
    pub id: String,
    pub account_id: String,
    pub token_digest: String,
    pub csrf_digest: String,
    pub idle_lifetime_seconds: u64,
    pub absolute_lifetime_seconds: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthenticatedSession {
    pub principal: AccountPrincipal,
    pub csrf_digest: String,
    pub idle_expires_at: String,
    pub absolute_expires_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthenticationEventKind {
    SignUp,
    Login,
    Logout,
    SessionExpired,
    PasswordRehashed,
}

impl AuthenticationEventKind {
    #[allow(dead_code)]
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::SignUp => "signup",
            Self::Login => "login",
            Self::Logout => "logout",
            Self::SessionExpired => "session_expired",
            Self::PasswordRehashed => "password_rehashed",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthenticationOutcomeClass {
    Success,
    InvalidCredentials,
    Throttled,
    InvalidRequest,
    InternalFailure,
}

impl AuthenticationOutcomeClass {
    #[allow(dead_code)]
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::InvalidCredentials => "invalid_credentials",
            Self::Throttled => "throttled",
            Self::InvalidRequest => "invalid_request",
            Self::InternalFailure => "internal_failure",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthenticationActionKind {
    Login,
    SignUp,
}

impl AuthenticationActionKind {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Login => "login",
            Self::SignUp => "signup",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthenticationAudit {
    pub id: String,
    pub account_id: Option<String>,
    pub event_kind: AuthenticationEventKind,
    pub outcome_class: AuthenticationOutcomeClass,
    pub correlation_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthenticationThrottleBucket {
    pub attempt_count: u32,
    pub blocked_until: Option<String>,
}

#[derive(Clone)]
pub struct AuthService {
    repository: MongoAccountRepository,
    cache: CacheService,
    email_crypto: EmailCrypto,
    config: Arc<AuthenticationConfig>,
    argon2: Argon2<'static>,
    dummy_password_phc: PasswordPhc,
    hash_permits: Arc<Semaphore>,
}

pub struct AuthenticationSecret(String);

impl AuthenticationSecret {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Exposes a raw value only to trusted cookie, CSRF, or one-time CLI output.
    pub fn expose_secret(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for AuthenticationSecret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("AuthenticationSecret([REDACTED])")
    }
}

impl Drop for AuthenticationSecret {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

#[derive(Debug)]
pub struct IssuedSession {
    pub account: AccountSummary,
    pub principal: AccountPrincipal,
    pub session_token: AuthenticationSecret,
    pub csrf_token: AuthenticationSecret,
    pub idle_expires_at: String,
    pub absolute_expires_at: String,
}

#[derive(Debug)]
pub struct IssuedSignupAccessToken {
    pub id: String,
    pub token: AuthenticationSecret,
    pub expires_at: String,
}

#[derive(Debug)]
pub struct IssuedSignupSession {
    pub id: String,
    pub session_token: AuthenticationSecret,
    pub csrf_token: AuthenticationSecret,
    pub expires_at: String,
}

impl AuthService {
    pub fn new(
        repository: MongoAccountRepository,
        cache: CacheService,
        config: AuthenticationConfig,
    ) -> Result<Self, AuthenticationError> {
        if config.session_idle_lifetime.is_zero()
            || config.session_absolute_lifetime.is_zero()
            || config.session_idle_lifetime > config.session_absolute_lifetime
            || config.max_active_sessions == 0
            || config.max_hash_concurrency == 0
            || config.throttle_hmac_key.expose_secret().len() < 32
        {
            return Err(AuthenticationError::InvalidInput(
                AuthenticationInputError::InvalidConfiguration,
            ));
        }
        let params = Params::new(
            config.argon2_memory_kib,
            config.argon2_iterations,
            config.argon2_parallelism,
            Some(32),
        )
        .map_err(|_| AuthenticationError::PasswordHash)?;
        let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
        let dummy_password_phc = hash_password_with(&argon2, "dummy-password-not-a-login")?;
        let hash_permits = Arc::new(Semaphore::new(config.max_hash_concurrency));
        let email_crypto = EmailCrypto::from_config(&config)?;
        Ok(Self {
            repository,
            cache,
            email_crypto,
            config: Arc::new(config),
            argon2,
            dummy_password_phc,
            hash_permits,
        })
    }

    /// Legacy direct signup is intentionally closed. Admission must first
    /// create a signup session through `begin_signup`.
    pub async fn sign_up(
        &self,
        _email: &str,
        _display_name: &str,
        _password: &AuthenticationSecret,
    ) -> Result<IssuedSession, AuthenticationError> {
        Err(AuthenticationError::AccountUnavailable)
    }

    pub async fn issue_signup_access_token(
        &self,
        allowed_role: &str,
        issued_by: &str,
        lifetime: Duration,
    ) -> Result<IssuedSignupAccessToken, AuthenticationError> {
        if !matches!(allowed_role, "user" | "admin")
            || !valid_opaque_identifier(issued_by)
            || lifetime.is_zero()
            || lifetime > MAX_ACCESS_TOKEN_LIFETIME
        {
            return Err(AuthenticationError::InvalidSignupAccess);
        }
        let token = random_token()?;
        let now = DateTime::now();
        let expires_at = add_duration(now, lifetime);
        let id = format!("signup-access-token:{}", Uuid::new_v4());
        self.repository
            .issue_signup_access_token(NewSignupAccessToken {
                id: id.clone(),
                token_digest: sha256_digest(token.expose_secret().as_bytes()),
                allowed_role: allowed_role.to_owned(),
                issued_by: issued_by.to_owned(),
                expires_at,
                purge_at: purge_after(expires_at),
            })
            .await
            .map_err(map_access_token_persistence)?;
        Ok(IssuedSignupAccessToken {
            id,
            token,
            expires_at: date_string(expires_at)?,
        })
    }

    pub async fn begin_signup(
        &self,
        access_token: &AuthenticationSecret,
    ) -> Result<IssuedSignupSession, AuthenticationError> {
        let throttle_identifier = sha256_digest(access_token.expose_secret().as_bytes());
        let throttle_digest =
            self.throttle_key_digest(&throttle_identifier, AuthenticationActionKind::SignUp);
        let bucket = self
            .record_authentication_attempt(&throttle_digest, AuthenticationActionKind::SignUp)
            .await?;
        if Self::is_throttled(&bucket) {
            return Err(AuthenticationError::Throttled);
        }
        let session_token = random_token()?;
        let csrf_token = random_token()?;
        let now = DateTime::now();
        let expires_at = add_duration(now, SIGNUP_SESSION_LIFETIME);
        let id = format!("signup-session:{}", Uuid::new_v4());
        self.repository
            .begin_signup(NewSignupSession {
                id: id.clone(),
                bearer_digest: sha256_digest(session_token.expose_secret().as_bytes()),
                csrf_digest: sha256_digest(csrf_token.expose_secret().as_bytes()),
                access_token_digest: sha256_digest(access_token.expose_secret().as_bytes()),
                expires_at,
                purge_at: purge_after(expires_at),
            })
            .await
            .map_err(map_access_token_persistence)?;
        Ok(IssuedSignupSession {
            id,
            session_token,
            csrf_token,
            expires_at: date_string(expires_at)?,
        })
    }

    pub async fn revoke_signup_access_token(
        &self,
        token_id: &str,
    ) -> Result<(), AuthenticationError> {
        if !token_id.starts_with("signup-access-token:") {
            return Err(AuthenticationError::InvalidSignupAccess);
        }
        if self.repository.revoke_signup_access_token(token_id).await? {
            Ok(())
        } else {
            Err(AuthenticationError::InvalidSignupAccess)
        }
    }

    pub async fn complete_signup(
        &self,
        signup_session_token: &AuthenticationSecret,
        signup_csrf_token: &AuthenticationSecret,
        email: &str,
        display_name: &str,
        password: &AuthenticationSecret,
    ) -> Result<IssuedSession, AuthenticationError> {
        let normalized_email =
            EmailCrypto::normalize(email).map_err(|_| AuthenticationError::AccountUnavailable)?;
        let username_normalized = normalize_username(display_name)?;
        validate_password(password.expose_secret())?;
        let password_phc = self.hash_password(password).await?;
        let account_id = format!("account:{}", Uuid::new_v4());
        let email_lookup_hmac = self.email_crypto.lookup_hmac(&normalized_email)?;
        let email_ciphertext = self
            .email_crypto
            .encrypt(&account_id, 1, &normalized_email)?;
        let (raw_session, account_session) = self.new_account_session(&account_id, 1)?;
        let outcome = self
            .repository
            .complete_signup(CompleteSignupWrite {
                signup_bearer_digest: sha256_digest(
                    signup_session_token.expose_secret().as_bytes(),
                ),
                signup_csrf_digest: sha256_digest(signup_csrf_token.expose_secret().as_bytes()),
                account: NewMongoAccount {
                    id: account_id,
                    username: display_name.to_owned(),
                    username_normalized,
                    email_ciphertext,
                    email_lookup_hmac,
                    password_phc,
                },
                account_session,
            })
            .await
            .map_err(map_complete_signup_persistence)?;
        self.cache_session(
            &raw_session.0,
            &outcome.session,
            DateTime::now().timestamp_millis(),
        )
        .await;
        Ok(issued_session(
            outcome.account,
            outcome.session.authenticated,
            raw_session,
        ))
    }

    /// `identifier` accepts either username or email. Failure remains generic.
    pub async fn login(
        &self,
        identifier: &str,
        password: &AuthenticationSecret,
    ) -> Result<IssuedSession, AuthenticationError> {
        let login_identifier = LoginIdentifier::parse(identifier, &self.email_crypto).ok();
        let throttle_identifier = login_identifier
            .as_ref()
            .map_or("invalid-login-identifier", LoginIdentifier::normalized);
        let throttle_digest =
            self.throttle_key_digest(throttle_identifier, AuthenticationActionKind::Login);
        let bucket = self
            .record_authentication_attempt(&throttle_digest, AuthenticationActionKind::Login)
            .await?;
        if Self::is_throttled(&bucket) {
            return Err(AuthenticationError::Throttled);
        }

        let account = match login_identifier.as_ref() {
            Some(LoginIdentifier::Email { lookup_hmac, .. }) => {
                self.repository
                    .load_login_account(None, Some(lookup_hmac))
                    .await?
            }
            Some(LoginIdentifier::Username(normalized)) => {
                self.repository
                    .load_login_account(Some(normalized), None)
                    .await?
            }
            None => None,
        };
        let candidate_phc = account
            .as_ref()
            .map_or(&self.dummy_password_phc, |account| &account.password_phc);
        let verified = self
            .verify_password(candidate_phc.clone(), password)
            .await?;
        let Some(mut account) = account.filter(|account| account.login_enabled && verified) else {
            return Err(AuthenticationError::InvalidCredentials);
        };

        if password_needs_rehash(&account.password_phc, &self.config) {
            let replacement = self.hash_password(password).await?;
            let updated = self
                .repository
                .update_password_and_revoke_sessions(&account.id, &replacement)
                .await?;
            account.password_role_version = updated.password_role_version;
            for digest in updated.revoked_bearer_digests {
                let _ = self.cache.del_session(&digest).await;
            }
        }
        let (raw_session, new_session) =
            self.new_account_session(&account.id, account.password_role_version)?;
        let outcome = self
            .repository
            .create_login_session(new_session, self.config.max_active_sessions)
            .await?;
        for digest in outcome.revoked_bearer_digests {
            let _ = self.cache.del_session(&digest).await;
        }
        self.cache_session(
            &raw_session.0,
            &outcome.session,
            DateTime::now().timestamp_millis(),
        )
        .await;
        Ok(issued_session(
            account_summary(&account)?,
            outcome.session.authenticated,
            raw_session,
        ))
    }

    pub async fn authenticate(
        &self,
        raw_session_token: &AuthenticationSecret,
    ) -> Result<AuthenticatedSession, AuthenticationError> {
        let digest = sha256_digest(raw_session_token.expose_secret().as_bytes());
        let now_millis = DateTime::now().timestamp_millis();
        if let Some(entry) = self.cache.get_session(&digest).await {
            if valid_cached_session(&entry, now_millis) {
                if self
                    .repository
                    .account_version_is_current(&entry.account_id, entry.password_role_version)
                    .await?
                {
                    let refresh_interval = self
                        .config
                        .session_idle_lifetime
                        .checked_div(2)
                        .unwrap_or(Duration::from_secs(1))
                        .min(Duration::from_secs(60))
                        .max(Duration::from_secs(1));
                    if now_millis.saturating_sub(entry.last_persisted_at_millis)
                        < duration_millis(refresh_interval)
                    {
                        return cached_authenticated_session(&entry);
                    }
                } else {
                    let _ = self.cache.del_session(&digest).await;
                }
            } else {
                let _ = self.cache.del_session(&digest).await;
            }
        }
        let session = self
            .repository
            .authenticate_session_digest(&digest, self.config.session_idle_lifetime)
            .await?
            .ok_or(AuthenticationError::InvalidSession)?;
        self.cache_session(raw_session_token, &session, now_millis)
            .await;
        Ok(session.authenticated)
    }

    pub async fn logout(&self, principal: &AccountPrincipal) -> Result<(), AuthenticationError> {
        self.revoke_session(&principal.account_id, &principal.session_id)
            .await
    }

    pub async fn revoke_session(
        &self,
        account_id: &str,
        session_id: &str,
    ) -> Result<(), AuthenticationError> {
        let digest = self
            .repository
            .revoke_account_session(account_id, session_id)
            .await?
            .ok_or(AuthenticationError::InvalidSession)?;
        let _ = self.cache.del_session(&digest).await;
        Ok(())
    }

    pub async fn cleanup_expired_sessions(&self) -> Result<u64, AuthenticationError> {
        Ok(self.repository.cleanup_expired_account_sessions().await?)
    }

    pub async fn load_account_summary(
        &self,
        account_id: &str,
    ) -> Result<Option<AccountSummary>, AuthenticationError> {
        Ok(self.repository.load_account_summary(account_id).await?)
    }

    /// HMAC-SHA256 digest of a throttle identifier. Raw emails and IPs never
    /// enter DragonflyDB or MongoDB throttle keys.
    pub fn throttle_key_digest(
        &self,
        identifier: &str,
        action: AuthenticationActionKind,
    ) -> String {
        use hmac::{Hmac, Mac};
        let mut mac = Hmac::<Sha256>::new_from_slice(
            self.config.throttle_hmac_key.expose_secret().as_bytes(),
        )
        .expect("HMAC accepts any key length");
        mac.update(identifier.as_bytes());
        mac.update(&[0]);
        mac.update(action.as_str().as_bytes());
        format!("hmac-sha256:{:x}", mac.finalize().into_bytes())
    }

    pub fn throttle_config(&self) -> (u64, u32, u64) {
        (
            self.config.throttle_window_seconds,
            self.config.throttle_block_after_attempts,
            self.config.throttle_block_seconds,
        )
    }

    pub async fn record_authentication_attempt(
        &self,
        key_digest: &str,
        action: AuthenticationActionKind,
    ) -> Result<AuthenticationThrottleBucket, AuthenticationError> {
        if !valid_hmac_digest(key_digest) {
            return Err(AuthenticationError::InvalidInput(
                AuthenticationInputError::InvalidThrottleDigest,
            ));
        }
        let (window_seconds, threshold, block_seconds) = self.throttle_config();
        let ttl = Duration::from_secs(window_seconds.max(block_seconds));
        match self
            .cache
            .increment_throttle(action.as_str(), key_digest, ttl)
            .await
        {
            Ok(Some(count)) => {
                let count = u32::try_from(count).unwrap_or(u32::MAX);
                let blocked_until = if count >= threshold {
                    Some(date_string(add_duration(
                        DateTime::now(),
                        Duration::from_secs(block_seconds),
                    ))?)
                } else {
                    None
                };
                Ok(AuthenticationThrottleBucket {
                    attempt_count: count,
                    blocked_until,
                })
            }
            Ok(None) | Err(_) => Ok(self
                .repository
                .record_authentication_attempt(
                    key_digest,
                    action,
                    Duration::from_secs(window_seconds),
                    threshold,
                    Duration::from_secs(block_seconds),
                )
                .await?),
        }
    }

    pub fn is_throttled(bucket: &AuthenticationThrottleBucket) -> bool {
        bucket.blocked_until.is_some()
    }

    async fn hash_password(
        &self,
        password: &AuthenticationSecret,
    ) -> Result<PasswordPhc, AuthenticationError> {
        let permit = self
            .hash_permits
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| AuthenticationError::PasswordHash)?;
        let argon2 = self.argon2.clone();
        let mut password = password.expose_secret().to_owned();
        tokio::task::spawn_blocking(move || {
            let result = hash_password_with(&argon2, &password);
            password.zeroize();
            drop(permit);
            result
        })
        .await
        .map_err(|_| AuthenticationError::PasswordHash)?
    }

    async fn verify_password(
        &self,
        password_phc: PasswordPhc,
        password: &AuthenticationSecret,
    ) -> Result<bool, AuthenticationError> {
        let permit = self
            .hash_permits
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| AuthenticationError::PasswordHash)?;
        let argon2 = self.argon2.clone();
        let mut password = password.expose_secret().to_owned();
        tokio::task::spawn_blocking(move || {
            let verified = verify_password_with(&argon2, &password_phc, &password);
            password.zeroize();
            drop(permit);
            verified
        })
        .await
        .map_err(|_| AuthenticationError::PasswordHash)
    }

    fn new_account_session(
        &self,
        account_id: &str,
        password_role_version: u32,
    ) -> Result<
        (
            (AuthenticationSecret, AuthenticationSecret),
            NewMongoAccountSession,
        ),
        AuthenticationError,
    > {
        let session_token = random_token()?;
        let csrf_token = random_token()?;
        let now = DateTime::now();
        let absolute_expires_at = add_duration(now, self.config.session_absolute_lifetime);
        let idle_expires_at = add_duration(now, self.config.session_idle_lifetime);
        let session = NewMongoAccountSession {
            id: format!("session:{}", Uuid::new_v4()),
            account_id: account_id.to_owned(),
            bearer_digest: sha256_digest(session_token.expose_secret().as_bytes()),
            csrf_digest: sha256_digest(csrf_token.expose_secret().as_bytes()),
            password_role_version,
            idle_expires_at,
            absolute_expires_at,
            purge_at: purge_after(absolute_expires_at),
        };
        Ok(((session_token, csrf_token), session))
    }

    async fn cache_session(
        &self,
        raw_session_token: &AuthenticationSecret,
        session: &MongoSessionRecord,
        persisted_at_millis: i64,
    ) {
        let digest = sha256_digest(raw_session_token.expose_secret().as_bytes());
        let Ok(idle_expires_at) =
            DateTime::parse_rfc3339_str(&session.authenticated.idle_expires_at)
        else {
            return;
        };
        let Ok(absolute_expires_at) =
            DateTime::parse_rfc3339_str(&session.authenticated.absolute_expires_at)
        else {
            return;
        };
        let now = DateTime::now().timestamp_millis();
        let remaining_millis = idle_expires_at
            .timestamp_millis()
            .min(absolute_expires_at.timestamp_millis())
            .saturating_sub(now);
        if remaining_millis <= 0 {
            return;
        }
        let ttl = SESSION_CACHE_MAX_TTL.min(Duration::from_millis(
            u64::try_from(remaining_millis).unwrap_or(1),
        ));
        let entry = SessionCacheEntry {
            account_id: session.authenticated.principal.account_id.clone(),
            session_id: session.authenticated.principal.session_id.clone(),
            role: session.role.clone(),
            csrf_digest: session.authenticated.csrf_digest.clone(),
            idle_expires_at_millis: idle_expires_at.timestamp_millis(),
            absolute_expires_at_millis: absolute_expires_at.timestamp_millis(),
            password_role_version: session.password_role_version,
            last_persisted_at_millis: session
                .last_seen_at
                .timestamp_millis()
                .max(persisted_at_millis),
        };
        let _ = self.cache.set_session(&digest, &entry, ttl).await;
    }
}

enum LoginIdentifier {
    Email {
        normalized: String,
        lookup_hmac: String,
    },
    Username(String),
}

impl LoginIdentifier {
    fn parse(identifier: &str, crypto: &EmailCrypto) -> Result<Self, AuthenticationError> {
        if identifier.contains('@') {
            let normalized = EmailCrypto::normalize(identifier)
                .map_err(|_| AuthenticationError::InvalidCredentials)?;
            let lookup_hmac = crypto.lookup_hmac(&normalized)?;
            Ok(Self::Email {
                normalized,
                lookup_hmac,
            })
        } else {
            Ok(Self::Username(normalize_username(identifier)?))
        }
    }

    fn normalized(&self) -> &str {
        match self {
            Self::Email { normalized, .. } | Self::Username(normalized) => normalized,
        }
    }
}

fn issued_session(
    account: AccountSummary,
    authenticated: AuthenticatedSession,
    raw: (AuthenticationSecret, AuthenticationSecret),
) -> IssuedSession {
    IssuedSession {
        account,
        principal: authenticated.principal,
        session_token: raw.0,
        csrf_token: raw.1,
        idle_expires_at: authenticated.idle_expires_at,
        absolute_expires_at: authenticated.absolute_expires_at,
    }
}

fn account_summary(account: &MongoLoginAccount) -> Result<AccountSummary, AuthenticationError> {
    Ok(AccountSummary {
        id: account.id.clone(),
        display_name: account.username.clone(),
        login_enabled: account.login_enabled,
        created_at: date_string(account.created_at)?,
        updated_at: date_string(account.updated_at)?,
    })
}

fn cached_authenticated_session(
    entry: &SessionCacheEntry,
) -> Result<AuthenticatedSession, AuthenticationError> {
    Ok(AuthenticatedSession {
        principal: AccountPrincipal {
            account_id: entry.account_id.clone(),
            session_id: entry.session_id.clone(),
        },
        csrf_digest: entry.csrf_digest.clone(),
        idle_expires_at: date_string(DateTime::from_millis(entry.idle_expires_at_millis))?,
        absolute_expires_at: date_string(DateTime::from_millis(entry.absolute_expires_at_millis))?,
    })
}

fn valid_cached_session(entry: &SessionCacheEntry, now_millis: i64) -> bool {
    entry.account_id.starts_with("account:")
        && entry.session_id.starts_with("session:")
        && valid_sha256_digest(&entry.csrf_digest)
        && entry.password_role_version > 0
        && entry.idle_expires_at_millis > now_millis
        && entry.absolute_expires_at_millis > now_millis
}

fn hash_password_with(
    argon2: &Argon2<'_>,
    password: &str,
) -> Result<PasswordPhc, AuthenticationError> {
    let mut salt_bytes = [0_u8; 16];
    rand::rngs::OsRng
        .try_fill_bytes(&mut salt_bytes)
        .map_err(|_| AuthenticationError::Randomness)?;
    let salt =
        SaltString::encode_b64(&salt_bytes).map_err(|_| AuthenticationError::PasswordHash)?;
    let encoded = argon2
        .hash_password(password.as_bytes(), &salt)
        .map_err(|_| AuthenticationError::PasswordHash)?
        .to_string();
    salt_bytes.zeroize();
    PasswordPhc::parse(encoded).map_err(AuthenticationError::InvalidInput)
}

fn verify_password_with(argon2: &Argon2<'_>, phc: &PasswordPhc, password: &str) -> bool {
    PasswordHash::new(phc.expose_secret())
        .ok()
        .is_some_and(|hash| argon2.verify_password(password.as_bytes(), &hash).is_ok())
}

fn password_needs_rehash(phc: &PasswordPhc, config: &AuthenticationConfig) -> bool {
    let Ok(hash) = PasswordHash::new(phc.expose_secret()) else {
        return true;
    };
    hash.algorithm.as_str() != "argon2id"
        || hash.version != Some(19)
        || hash.params.get_decimal("m") != Some(config.argon2_memory_kib)
        || hash.params.get_decimal("t") != Some(config.argon2_iterations)
        || hash.params.get_decimal("p") != Some(config.argon2_parallelism)
}

fn normalize_username(raw: &str) -> Result<String, AuthenticationError> {
    let trimmed = raw.trim();
    let normalized = trimmed.to_ascii_lowercase();
    let valid = trimmed == raw
        && (1..=200).contains(&trimmed.len())
        && !normalized.is_empty()
        && !trimmed.chars().any(|c| c.is_control());
    valid
        .then_some(normalized)
        .ok_or(AuthenticationError::AccountUnavailable)
}

fn validate_password(password: &str) -> Result<(), AuthenticationError> {
    let scalar_count = password.chars().count();
    let common = matches!(
        password.to_ascii_lowercase().as_str(),
        "passwordpassword"
            | "password123456"
            | "correct horse battery staple"
            | "testtesttesttesttest"
    );
    if (15..=128).contains(&scalar_count) && !common {
        Ok(())
    } else {
        Err(AuthenticationError::AccountUnavailable)
    }
}

fn valid_opaque_identifier(value: &str) -> bool {
    (1..=128).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b':' | b'-'))
}

fn random_token() -> Result<AuthenticationSecret, AuthenticationError> {
    let mut bytes = [0_u8; 32];
    rand::rngs::OsRng
        .try_fill_bytes(&mut bytes)
        .map_err(|_| AuthenticationError::Randomness)?;
    let encoded = URL_SAFE_NO_PAD.encode(bytes);
    bytes.zeroize();
    Ok(AuthenticationSecret::new(encoded))
}

pub(crate) fn sha256_digest(value: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(value))
}

fn date_string(value: DateTime) -> Result<String, AuthenticationError> {
    value
        .try_to_rfc3339_string()
        .map_err(|_| AuthenticationError::InvalidSession)
}

fn duration_millis(duration: Duration) -> i64 {
    i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
}

fn map_access_token_persistence(error: PersistenceError) -> AuthenticationError {
    match error.mongo_failure_kind() {
        Some(MongoFailureKind::DuplicateKey) => AuthenticationError::InvalidSignupAccess,
        _ if matches!(error, PersistenceError::NotFound { .. }) => {
            AuthenticationError::InvalidSignupAccess
        }
        _ => AuthenticationError::Persistence(error),
    }
}

fn map_complete_signup_persistence(error: PersistenceError) -> AuthenticationError {
    match error.mongo_failure_kind() {
        Some(MongoFailureKind::DuplicateKey) => AuthenticationError::AccountUnavailable,
        _ if matches!(
            error,
            PersistenceError::NotFound { .. } | PersistenceError::RevisionConflict { .. }
        ) =>
        {
            AuthenticationError::InvalidSignupSession
        }
        _ => AuthenticationError::Persistence(error),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthenticationInputError {
    InvalidPasswordHash,
    InvalidConfiguration,
    InvalidThrottleDigest,
}

impl fmt::Display for AuthenticationInputError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("authentication input failed validation")
    }
}

impl std::error::Error for AuthenticationInputError {}

pub(crate) fn valid_sha256_digest(value: &str) -> bool {
    value.len() == SHA256_DIGEST_PREFIX.len() + 64
        && value.starts_with(SHA256_DIGEST_PREFIX)
        && value[SHA256_DIGEST_PREFIX.len()..]
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn valid_hmac_digest(value: &str) -> bool {
    value.len() == "hmac-sha256:".len() + 64
        && value.starts_with("hmac-sha256:")
        && value["hmac-sha256:".len()..]
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn password_policy_is_scalar_bounded_and_has_no_composition_rule() {
        assert!(validate_password("fifteenlettersok").is_ok());
        assert!(validate_password("all lowercase words are accepted here").is_ok());
        assert!(validate_password("short").is_err());
        assert!(validate_password(&"x".repeat(129)).is_err());
        assert!(validate_password("password123456").is_err());
    }

    #[test]
    fn hashes_use_random_salts_and_secrets_are_redacted() {
        let params = Params::new(19_456, 2, 1, Some(32)).unwrap();
        let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
        let first = hash_password_with(&argon2, "a sufficiently long passphrase").unwrap();
        let second = hash_password_with(&argon2, "a sufficiently long passphrase").unwrap();
        assert_ne!(first.expose_secret(), second.expose_secret());
        assert!(verify_password_with(
            &argon2,
            &first,
            "a sufficiently long passphrase"
        ));
        assert!(!verify_password_with(&argon2, &first, "wrong password"));
        assert_eq!(format!("{first:?}"), "PasswordPhc([REDACTED])");
        let secret = AuthenticationSecret::new("never-print-this");
        assert_eq!(format!("{secret:?}"), "AuthenticationSecret([REDACTED])");
    }

    #[test]
    fn cache_validation_rejects_expired_or_malformed_entries() {
        let now = DateTime::now().timestamp_millis();
        let valid = SessionCacheEntry {
            account_id: format!("account:{}", Uuid::new_v4()),
            session_id: format!("session:{}", Uuid::new_v4()),
            role: "user".to_owned(),
            csrf_digest: sha256_digest(b"csrf"),
            idle_expires_at_millis: now + 1_000,
            absolute_expires_at_millis: now + 2_000,
            password_role_version: 1,
            last_persisted_at_millis: now,
        };
        assert!(valid_cached_session(&valid, now));
        assert!(!valid_cached_session(
            &SessionCacheEntry {
                idle_expires_at_millis: now,
                ..valid.clone()
            },
            now
        ));
        assert!(!valid_cached_session(
            &SessionCacheEntry {
                csrf_digest: "raw-csrf".to_owned(),
                ..valid
            },
            now
        ));
    }
}
