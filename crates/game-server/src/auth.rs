//! Account and opaque server-side session domain types.
//!
//! Password verification and raw token generation belong to `AuthService` in
//! the next phase. These types deliberately keep hashes redacted from `Debug`.

use std::{fmt, sync::Arc};

use argon2::{
    Algorithm, Argon2, Params, Version,
    password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use rand::TryRngCore;
use sha2::{Digest, Sha256};
use tokio::sync::Semaphore;
use uuid::Uuid;
use zeroize::Zeroize;

use crate::{
    config::AuthenticationConfig,
    error::{AuthenticationError, RepositoryError},
    repository::PostgresRepository,
};

use serde::{Deserialize, Serialize};

pub const LOCAL_ACCOUNT_ID: &str = "account:local";
pub const SHA256_DIGEST_PREFIX: &str = "sha256:";

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
    repository: PostgresRepository,
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

    /// Exposes the raw value only to the trusted HTTP cookie/CSRF boundary.
    /// Callers must never serialize it into logs, URLs, local storage, or
    /// Leptos shared state.
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

impl AuthService {
    pub fn new(
        repository: PostgresRepository,
        config: AuthenticationConfig,
    ) -> Result<Self, AuthenticationError> {
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
        Ok(Self {
            repository,
            config: Arc::new(config),
            argon2,
            dummy_password_phc,
            hash_permits,
        })
    }

    pub async fn sign_up(
        &self,
        email: &str,
        display_name: &str,
        password: &AuthenticationSecret,
    ) -> Result<IssuedSession, AuthenticationError> {
        let normalized_email = normalize_email(email)?;
        validate_display_name(display_name)?;
        validate_password(password.expose_secret())?;
        let password_phc = self.hash_password(password).await?;
        let account_id = format!("account:{}", Uuid::new_v4());
        let (raw_session, new_session) = self.new_session(&account_id)?;
        let new_account = NewAccount {
            id: account_id,
            normalized_email,
            display_name: display_name.to_owned(),
            password_phc,
        };
        let (account, authenticated) = self
            .repository
            .create_account_with_session(&new_account, &new_session)
            .await
            .map_err(|error| match error {
                RepositoryError::AlreadyExists { .. } => AuthenticationError::AccountUnavailable,
                other => AuthenticationError::Repository(other),
            })?;
        Ok(issued_session(account, authenticated, raw_session))
    }

    pub async fn login(
        &self,
        email: &str,
        password: &AuthenticationSecret,
    ) -> Result<IssuedSession, AuthenticationError> {
        let normalized_email = normalize_email(email).ok();
        let account = match normalized_email {
            Some(ref email) => self.repository.load_login_account(email).await?,
            None => None,
        };
        let candidate_phc = account
            .as_ref()
            .map_or(&self.dummy_password_phc, |account| &account.password_phc);
        let verified = self
            .verify_password(candidate_phc.clone(), password)
            .await?;
        let Some(account) = account.filter(|account| account.login_enabled && verified) else {
            return Err(AuthenticationError::InvalidCredentials);
        };
        if password_needs_rehash(&account.password_phc, &self.config) {
            let replacement = self.hash_password(password).await?;
            self.repository
                .update_account_password_phc(&account.id, &replacement)
                .await?;
        }
        let (raw_session, new_session) = self.new_session(&account.id)?;
        let authenticated = self.repository.create_account_session(&new_session).await?;
        self.repository
            .revoke_excess_account_sessions(&account.id, self.config.max_active_sessions)
            .await?;
        let summary = AccountSummary {
            id: account.id,
            display_name: account.display_name,
            login_enabled: true,
            created_at: account.created_at,
            updated_at: account.updated_at,
        };
        Ok(issued_session(summary, authenticated, raw_session))
    }

    pub async fn authenticate(
        &self,
        raw_session_token: &AuthenticationSecret,
    ) -> Result<AuthenticatedSession, AuthenticationError> {
        self.repository
            .authenticate_session_digest(
                &sha256_digest(raw_session_token.expose_secret().as_bytes()),
                self.config.session_idle_lifetime.as_secs(),
            )
            .await?
            .ok_or(AuthenticationError::InvalidSession)
    }

    pub async fn logout(&self, principal: &AccountPrincipal) -> Result<(), AuthenticationError> {
        if self
            .repository
            .revoke_account_session(&principal.account_id, &principal.session_id)
            .await?
        {
            Ok(())
        } else {
            Err(AuthenticationError::InvalidSession)
        }
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

    /// HMAC-SHA256 digest of a throttle identifier. Raw emails and IPs are
    /// never persisted in throttle buckets.
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
        let (window, threshold, block) = self.throttle_config();
        Ok(self
            .repository
            .record_authentication_attempt(key_digest, action, window, threshold, block)
            .await?)
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

    fn new_session(
        &self,
        account_id: &str,
    ) -> Result<
        (
            (AuthenticationSecret, AuthenticationSecret),
            NewAccountSession,
        ),
        AuthenticationError,
    > {
        let session_token = random_token()?;
        let csrf_token = random_token()?;
        let session = NewAccountSession {
            id: format!("session:{}", Uuid::new_v4()),
            account_id: account_id.to_owned(),
            token_digest: sha256_digest(session_token.expose_secret().as_bytes()),
            csrf_digest: sha256_digest(csrf_token.expose_secret().as_bytes()),
            idle_lifetime_seconds: self.config.session_idle_lifetime.as_secs(),
            absolute_lifetime_seconds: self.config.session_absolute_lifetime.as_secs(),
        };
        Ok(((session_token, csrf_token), session))
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

fn normalize_email(raw: &str) -> Result<String, AuthenticationError> {
    let normalized = raw.trim().to_lowercase();
    let valid = (3..=320).contains(&normalized.len())
        && !normalized.chars().any(char::is_whitespace)
        && normalized.matches('@').count() == 1
        && normalized.split('@').all(|part| !part.is_empty());
    if valid {
        Ok(normalized)
    } else {
        Err(AuthenticationError::InvalidCredentials)
    }
}

fn validate_display_name(display_name: &str) -> Result<(), AuthenticationError> {
    if display_name.trim() == display_name && (1..=200).contains(&display_name.len()) {
        Ok(())
    } else {
        Err(AuthenticationError::AccountUnavailable)
    }
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

fn random_token() -> Result<AuthenticationSecret, AuthenticationError> {
    let mut bytes = [0_u8; 32];
    rand::rngs::OsRng
        .try_fill_bytes(&mut bytes)
        .map_err(|_| AuthenticationError::Randomness)?;
    let encoded = URL_SAFE_NO_PAD.encode(bytes);
    bytes.zeroize();
    Ok(AuthenticationSecret::new(encoded))
}

fn sha256_digest(value: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(value))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthenticationInputError {
    InvalidPasswordHash,
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

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use sqlx::{PgPool, Row};

    use super::*;
    use crate::repository::MIGRATOR;

    fn config() -> AuthenticationConfig {
        AuthenticationConfig {
            session_idle_lifetime: Duration::from_secs(60),
            session_absolute_lifetime: Duration::from_secs(600),
            max_active_sessions: 3,
            max_hash_concurrency: 2,
            throttle_window_seconds: 300,
            throttle_block_after_attempts: 5,
            throttle_block_seconds: 60,
            throttle_hmac_key: crate::config::SecretString::new("test-throttle-key"),
            cookie_secure: false,
            canonical_origin: None,
            argon2_memory_kib: 19_456,
            argon2_iterations: 2,
            argon2_parallelism: 1,
        }
    }

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

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn signup_login_authenticate_and_logout_keep_raw_secrets_out_of_postgres(pool: PgPool) {
        let service =
            AuthService::new(PostgresRepository::from_pool(pool.clone()), config()).unwrap();
        let password = AuthenticationSecret::new("a long local test passphrase");
        let issued = service
            .sign_up(" Player@Example.Test ", "Test Player", &password)
            .await
            .unwrap();
        assert_eq!(issued.account.display_name, "Test Player");
        let row = sqlx::query(
            "SELECT normalized_email, password_phc, token_digest
             FROM accounts JOIN account_sessions ON accounts.id = account_sessions.account_id
             WHERE accounts.id = $1",
        )
        .bind(&issued.principal.account_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(
            row.get::<String, _>("normalized_email"),
            "player@example.test"
        );
        let stored_phc: String = row.get("password_phc");
        assert!(stored_phc.starts_with("$argon2id$"));
        assert!(!stored_phc.contains(password.expose_secret()));
        assert_ne!(
            row.get::<String, _>("token_digest"),
            issued.session_token.expose_secret()
        );

        let authenticated = service.authenticate(&issued.session_token).await.unwrap();
        assert_eq!(authenticated.principal, issued.principal);
        service.logout(&authenticated.principal).await.unwrap();
        assert!(matches!(
            service.authenticate(&issued.session_token).await,
            Err(AuthenticationError::InvalidSession)
        ));

        let login = service
            .login("PLAYER@example.test", &password)
            .await
            .unwrap();
        assert_eq!(login.principal.account_id, issued.principal.account_id);
        assert!(matches!(
            service
                .login(
                    "PLAYER@example.test",
                    &AuthenticationSecret::new("a different wrong passphrase")
                )
                .await,
            Err(AuthenticationError::InvalidCredentials)
        ));
        assert!(matches!(
            service.login("unknown@example.test", &password).await,
            Err(AuthenticationError::InvalidCredentials)
        ));
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn successful_login_rehashes_parameters_below_policy(pool: PgPool) {
        let repository = PostgresRepository::from_pool(pool.clone());
        let weak_argon = Argon2::new(
            Algorithm::Argon2id,
            Version::V0x13,
            Params::new(8_192, 1, 1, Some(32)).unwrap(),
        );
        let password = AuthenticationSecret::new("another durable local passphrase");
        let weak_phc = hash_password_with(&weak_argon, password.expose_secret()).unwrap();
        let account_id = format!("account:{}", Uuid::new_v4());
        repository
            .create_account(&NewAccount {
                id: account_id.clone(),
                normalized_email: "rehash@example.test".to_owned(),
                display_name: "Rehash Test".to_owned(),
                password_phc: weak_phc,
            })
            .await
            .unwrap();
        let service = AuthService::new(repository, config()).unwrap();
        service
            .login("rehash@example.test", &password)
            .await
            .unwrap();
        let upgraded: String =
            sqlx::query_scalar("SELECT password_phc FROM accounts WHERE id = $1")
                .bind(account_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        let upgraded = PasswordPhc::parse(upgraded).unwrap();
        assert!(!password_needs_rehash(&upgraded, &config()));
    }
}
