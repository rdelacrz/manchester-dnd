//! Parameterized PostgreSQL access for accounts, sessions, throttles, and
//! body-free authentication audits.

use sqlx::Row;
use uuid::{Uuid, Version};

use super::PostgresRepository;
use crate::{
    auth::{
        AccountPrincipal, AccountSummary, AuthenticatedSession, AuthenticationActionKind,
        AuthenticationAudit, AuthenticationThrottleBucket, LoginAccount, NewAccount,
        NewAccountSession, PasswordPhc, valid_sha256_digest,
    },
    error::RepositoryError,
};

impl PostgresRepository {
    pub async fn create_account(
        &self,
        account: &NewAccount,
    ) -> Result<AccountSummary, RepositoryError> {
        validate_new_account(account)?;
        let row = sqlx::query(
            "INSERT INTO accounts
             (id, normalized_email, display_name, password_phc, login_enabled,
              password_changed_at)
             VALUES ($1, $2, $3, $4, TRUE, CURRENT_TIMESTAMP)
             RETURNING id, display_name, login_enabled,
                       created_at::text AS created_at, updated_at::text AS updated_at",
        )
        .bind(&account.id)
        .bind(&account.normalized_email)
        .bind(&account.display_name)
        .bind(account.password_phc.expose_secret())
        .fetch_one(&self.pool)
        .await
        .map_err(|error| map_auth_insert_error(error, "account", &account.id))?;
        account_summary_from_row(&row)
    }

    pub async fn create_account_with_session(
        &self,
        account: &NewAccount,
        session: &NewAccountSession,
    ) -> Result<(AccountSummary, AuthenticatedSession), RepositoryError> {
        validate_new_account(account)?;
        validate_new_session(session)?;
        if session.account_id != account.id {
            return invalid(
                "account session",
                &session.id,
                "session account does not match new account",
            );
        }
        let idle_seconds = to_positive_i64(session.idle_lifetime_seconds, "idle lifetime")?;
        let absolute_seconds =
            to_positive_i64(session.absolute_lifetime_seconds, "absolute lifetime")?;
        if idle_seconds > absolute_seconds {
            return invalid(
                "account session",
                &session.id,
                "idle lifetime cannot exceed absolute lifetime",
            );
        }
        let mut transaction = self.pool.begin().await.map_err(RepositoryError::Database)?;
        let account_row = sqlx::query(
            "INSERT INTO accounts
             (id, normalized_email, display_name, password_phc, login_enabled,
              password_changed_at)
             VALUES ($1, $2, $3, $4, TRUE, CURRENT_TIMESTAMP)
             RETURNING id, display_name, login_enabled,
                       created_at::text AS created_at, updated_at::text AS updated_at",
        )
        .bind(&account.id)
        .bind(&account.normalized_email)
        .bind(&account.display_name)
        .bind(account.password_phc.expose_secret())
        .fetch_one(&mut *transaction)
        .await
        .map_err(|error| map_auth_insert_error(error, "account", &account.id))?;
        let session_row = sqlx::query(
            "INSERT INTO account_sessions
             (id, account_id, token_digest, csrf_digest, idle_expires_at,
              absolute_expires_at)
             VALUES ($1, $2, $3, $4,
                     CURRENT_TIMESTAMP + make_interval(secs => $5),
                     CURRENT_TIMESTAMP + make_interval(secs => $6))
             RETURNING id, account_id, csrf_digest,
                       idle_expires_at::text AS idle_expires_at,
                       absolute_expires_at::text AS absolute_expires_at",
        )
        .bind(&session.id)
        .bind(&session.account_id)
        .bind(&session.token_digest)
        .bind(&session.csrf_digest)
        .bind(idle_seconds)
        .bind(absolute_seconds)
        .fetch_one(&mut *transaction)
        .await
        .map_err(|error| map_auth_insert_error(error, "account session", &session.id))?;
        let outcome = (
            account_summary_from_row(&account_row)?,
            authenticated_session_from_row(&session_row)?,
        );
        transaction
            .commit()
            .await
            .map_err(RepositoryError::Database)?;
        Ok(outcome)
    }

    pub async fn load_login_account(
        &self,
        normalized_email: &str,
    ) -> Result<Option<LoginAccount>, RepositoryError> {
        validate_normalized_email(normalized_email)?;
        let row = sqlx::query(
            "SELECT id, display_name, password_phc, login_enabled,
                    created_at::text AS created_at, updated_at::text AS updated_at
             FROM accounts
             WHERE normalized_email = $1",
        )
        .bind(normalized_email)
        .fetch_optional(&self.pool)
        .await
        .map_err(RepositoryError::Database)?;
        row.map(|row| {
            let id: String = row.try_get("id").map_err(RepositoryError::Database)?;
            let password_phc: String = row
                .try_get("password_phc")
                .map_err(RepositoryError::Database)?;
            Ok(LoginAccount {
                id: id.clone(),
                display_name: row
                    .try_get("display_name")
                    .map_err(RepositoryError::Database)?,
                password_phc: PasswordPhc::parse(password_phc).map_err(|_| {
                    RepositoryError::InvalidDomainState {
                        entity: "account",
                        id,
                        reason: "stored password hash is invalid",
                    }
                })?,
                login_enabled: row
                    .try_get("login_enabled")
                    .map_err(RepositoryError::Database)?,
                created_at: row
                    .try_get("created_at")
                    .map_err(RepositoryError::Database)?,
                updated_at: row
                    .try_get("updated_at")
                    .map_err(RepositoryError::Database)?,
            })
        })
        .transpose()
    }

    pub async fn load_account_summary(
        &self,
        account_id: &str,
    ) -> Result<Option<AccountSummary>, RepositoryError> {
        validate_account_id(account_id)?;
        let row = sqlx::query(
            "SELECT id, display_name, login_enabled,
                    created_at::text AS created_at, updated_at::text AS updated_at
             FROM accounts
             WHERE id = $1 AND login_enabled = TRUE",
        )
        .bind(account_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(RepositoryError::Database)?;
        row.map(|row| account_summary_from_row(&row)).transpose()
    }

    pub async fn create_account_session(
        &self,
        session: &NewAccountSession,
    ) -> Result<AuthenticatedSession, RepositoryError> {
        validate_new_session(session)?;
        let idle_seconds = to_positive_i64(session.idle_lifetime_seconds, "idle lifetime")?;
        let absolute_seconds =
            to_positive_i64(session.absolute_lifetime_seconds, "absolute lifetime")?;
        if idle_seconds > absolute_seconds {
            return invalid(
                "account session",
                &session.id,
                "idle lifetime cannot exceed absolute lifetime",
            );
        }
        let row = sqlx::query(
            "INSERT INTO account_sessions
             (id, account_id, token_digest, csrf_digest, idle_expires_at,
              absolute_expires_at)
             VALUES ($1, $2, $3, $4,
                     CURRENT_TIMESTAMP + make_interval(secs => $5),
                     CURRENT_TIMESTAMP + make_interval(secs => $6))
             RETURNING id, account_id, csrf_digest,
                       idle_expires_at::text AS idle_expires_at,
                       absolute_expires_at::text AS absolute_expires_at",
        )
        .bind(&session.id)
        .bind(&session.account_id)
        .bind(&session.token_digest)
        .bind(&session.csrf_digest)
        .bind(idle_seconds)
        .bind(absolute_seconds)
        .fetch_one(&self.pool)
        .await
        .map_err(|error| map_auth_insert_error(error, "account session", &session.id))?;
        authenticated_session_from_row(&row)
    }

    /// Returns only active, unexpired sessions and advances their bounded idle
    /// deadline without allowing it to exceed absolute expiry.
    pub async fn authenticate_session_digest(
        &self,
        token_digest: &str,
        idle_lifetime_seconds: u64,
    ) -> Result<Option<AuthenticatedSession>, RepositoryError> {
        if !valid_sha256_digest(token_digest) {
            return invalid("account session", "token-digest", "token digest is invalid");
        }
        let idle_seconds = to_positive_i64(idle_lifetime_seconds, "idle lifetime")?;
        let row = sqlx::query(
            "UPDATE account_sessions
             SET last_seen_at = CURRENT_TIMESTAMP,
                 idle_expires_at = LEAST(
                     absolute_expires_at,
                     CURRENT_TIMESTAMP + make_interval(secs => $2)
                 )
             WHERE token_digest = $1
               AND revoked_at IS NULL
               AND idle_expires_at > CURRENT_TIMESTAMP
               AND absolute_expires_at > CURRENT_TIMESTAMP
             RETURNING id, account_id, csrf_digest,
                       idle_expires_at::text AS idle_expires_at,
                       absolute_expires_at::text AS absolute_expires_at",
        )
        .bind(token_digest)
        .bind(idle_seconds)
        .fetch_optional(&self.pool)
        .await
        .map_err(RepositoryError::Database)?;
        row.map(|row| authenticated_session_from_row(&row))
            .transpose()
    }

    pub async fn update_account_password_phc(
        &self,
        account_id: &str,
        password_phc: &PasswordPhc,
    ) -> Result<(), RepositoryError> {
        validate_account_id(account_id)?;
        let result = sqlx::query(
            "UPDATE accounts
             SET password_phc = $2, password_changed_at = CURRENT_TIMESTAMP,
                 updated_at = CURRENT_TIMESTAMP
             WHERE id = $1 AND login_enabled = TRUE",
        )
        .bind(account_id)
        .bind(password_phc.expose_secret())
        .execute(&self.pool)
        .await
        .map_err(RepositoryError::Database)?;
        if result.rows_affected() != 1 {
            return Err(RepositoryError::NotFound {
                entity: "account",
                id: account_id.to_owned(),
            });
        }
        Ok(())
    }

    pub async fn revoke_excess_account_sessions(
        &self,
        account_id: &str,
        maximum_active: u32,
    ) -> Result<u64, RepositoryError> {
        validate_account_id(account_id)?;
        if maximum_active == 0 {
            return invalid(
                "account session",
                account_id,
                "active-session limit is invalid",
            );
        }
        let maximum = i64::from(maximum_active);
        let result = sqlx::query(
            "WITH excess AS (
                 SELECT id FROM account_sessions
                 WHERE account_id = $1 AND revoked_at IS NULL
                   AND idle_expires_at > CURRENT_TIMESTAMP
                   AND absolute_expires_at > CURRENT_TIMESTAMP
                 ORDER BY created_at DESC, id DESC
                 OFFSET $2
             )
             UPDATE account_sessions AS sessions
             SET revoked_at = CURRENT_TIMESTAMP
             FROM excess WHERE sessions.id = excess.id",
        )
        .bind(account_id)
        .bind(maximum)
        .execute(&self.pool)
        .await
        .map_err(RepositoryError::Database)?;
        Ok(result.rows_affected())
    }

    pub async fn revoke_account_session(
        &self,
        account_id: &str,
        session_id: &str,
    ) -> Result<bool, RepositoryError> {
        validate_account_id(account_id)?;
        validate_uuid_id(session_id, "session:", "account session")?;
        let result = sqlx::query(
            "UPDATE account_sessions
             SET revoked_at = COALESCE(revoked_at, CURRENT_TIMESTAMP)
             WHERE account_id = $1 AND id = $2 AND revoked_at IS NULL",
        )
        .bind(account_id)
        .bind(session_id)
        .execute(&self.pool)
        .await
        .map_err(RepositoryError::Database)?;
        Ok(result.rows_affected() == 1)
    }

    pub async fn cleanup_expired_account_sessions(&self) -> Result<u64, RepositoryError> {
        let result = sqlx::query(
            "DELETE FROM account_sessions
             WHERE absolute_expires_at <= CURRENT_TIMESTAMP
                OR idle_expires_at <= CURRENT_TIMESTAMP
                OR (revoked_at IS NOT NULL AND revoked_at <= CURRENT_TIMESTAMP - INTERVAL '30 days')",
        )
        .execute(&self.pool)
        .await
        .map_err(RepositoryError::Database)?;
        Ok(result.rows_affected())
    }

    pub async fn record_authentication_audit(
        &self,
        audit: &AuthenticationAudit,
    ) -> Result<(), RepositoryError> {
        validate_uuid_id(&audit.id, "auth-audit:", "authentication audit")?;
        if let Some(account_id) = audit.account_id.as_deref() {
            validate_account_id(account_id)?;
        }
        validate_bounded_opaque(&audit.correlation_id, "authentication audit", &audit.id)?;
        sqlx::query(
            "INSERT INTO authentication_audits
             (id, account_id, event_kind, outcome_class, correlation_id)
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(&audit.id)
        .bind(&audit.account_id)
        .bind(audit.event_kind.as_str())
        .bind(audit.outcome_class.as_str())
        .bind(&audit.correlation_id)
        .execute(&self.pool)
        .await
        .map_err(|error| map_auth_insert_error(error, "authentication audit", &audit.id))?;
        Ok(())
    }

    /// Upserts an HMAC-digested throttle key. No raw email or IP is accepted by
    /// this boundary.
    pub async fn record_authentication_attempt(
        &self,
        key_digest: &str,
        action: AuthenticationActionKind,
        window_seconds: u64,
        block_after_attempts: u32,
        block_seconds: u64,
    ) -> Result<AuthenticationThrottleBucket, RepositoryError> {
        validate_hmac_digest(key_digest)?;
        let window_seconds = to_positive_i64(window_seconds, "throttle window")?;
        let block_seconds = to_positive_i64(block_seconds, "throttle block")?;
        if block_after_attempts == 0 {
            return invalid(
                "authentication throttle",
                key_digest,
                "threshold is invalid",
            );
        }
        let threshold =
            i32::try_from(block_after_attempts).map_err(|_| RepositoryError::NumericRange {
                field: "throttle threshold",
            })?;
        let row = sqlx::query(
            "INSERT INTO auth_throttle_buckets
             (key_digest, action_kind, window_started_at, attempt_count, blocked_until)
             VALUES ($1, $2, CURRENT_TIMESTAMP, 1,
                     CASE WHEN $4 <= 1
                          THEN CURRENT_TIMESTAMP + make_interval(secs => $5)
                          ELSE NULL END)
             ON CONFLICT (key_digest, action_kind) DO UPDATE SET
                 window_started_at = CASE
                     WHEN auth_throttle_buckets.window_started_at
                          <= CURRENT_TIMESTAMP - make_interval(secs => $3)
                     THEN CURRENT_TIMESTAMP
                     ELSE auth_throttle_buckets.window_started_at END,
                 attempt_count = CASE
                     WHEN auth_throttle_buckets.window_started_at
                          <= CURRENT_TIMESTAMP - make_interval(secs => $3)
                     THEN 1 ELSE auth_throttle_buckets.attempt_count + 1 END,
                 blocked_until = CASE
                     WHEN (CASE
                         WHEN auth_throttle_buckets.window_started_at
                              <= CURRENT_TIMESTAMP - make_interval(secs => $3)
                         THEN 1 ELSE auth_throttle_buckets.attempt_count + 1 END) >= $4
                     THEN CURRENT_TIMESTAMP + make_interval(secs => $5)
                     ELSE auth_throttle_buckets.blocked_until END,
                 updated_at = CURRENT_TIMESTAMP
             RETURNING attempt_count, blocked_until::text AS blocked_until",
        )
        .bind(key_digest)
        .bind(action.as_str())
        .bind(window_seconds)
        .bind(threshold)
        .bind(block_seconds)
        .fetch_one(&self.pool)
        .await
        .map_err(RepositoryError::Database)?;
        let count: i32 = row
            .try_get("attempt_count")
            .map_err(RepositoryError::Database)?;
        Ok(AuthenticationThrottleBucket {
            attempt_count: u32::try_from(count).map_err(|_| RepositoryError::NumericRange {
                field: "authentication attempt count",
            })?,
            blocked_until: row
                .try_get("blocked_until")
                .map_err(RepositoryError::Database)?,
        })
    }
}

fn account_summary_from_row(
    row: &sqlx::postgres::PgRow,
) -> Result<AccountSummary, RepositoryError> {
    Ok(AccountSummary {
        id: row.try_get("id").map_err(RepositoryError::Database)?,
        display_name: row
            .try_get("display_name")
            .map_err(RepositoryError::Database)?,
        login_enabled: row
            .try_get("login_enabled")
            .map_err(RepositoryError::Database)?,
        created_at: row
            .try_get("created_at")
            .map_err(RepositoryError::Database)?,
        updated_at: row
            .try_get("updated_at")
            .map_err(RepositoryError::Database)?,
    })
}

fn authenticated_session_from_row(
    row: &sqlx::postgres::PgRow,
) -> Result<AuthenticatedSession, RepositoryError> {
    Ok(AuthenticatedSession {
        principal: AccountPrincipal {
            account_id: row
                .try_get("account_id")
                .map_err(RepositoryError::Database)?,
            session_id: row.try_get("id").map_err(RepositoryError::Database)?,
        },
        csrf_digest: row
            .try_get("csrf_digest")
            .map_err(RepositoryError::Database)?,
        idle_expires_at: row
            .try_get("idle_expires_at")
            .map_err(RepositoryError::Database)?,
        absolute_expires_at: row
            .try_get("absolute_expires_at")
            .map_err(RepositoryError::Database)?,
    })
}

fn validate_new_account(account: &NewAccount) -> Result<(), RepositoryError> {
    validate_account_id(&account.id)?;
    if account.id == crate::auth::LOCAL_ACCOUNT_ID {
        return invalid(
            "account",
            &account.id,
            "local account cannot be a login account",
        );
    }
    validate_normalized_email(&account.normalized_email)?;
    if account.display_name.trim() != account.display_name
        || !(1..=200).contains(&account.display_name.len())
    {
        return invalid("account", &account.id, "display name is invalid");
    }
    Ok(())
}

fn validate_new_session(session: &NewAccountSession) -> Result<(), RepositoryError> {
    validate_uuid_id(&session.id, "session:", "account session")?;
    validate_account_id(&session.account_id)?;
    if !valid_sha256_digest(&session.token_digest) || !valid_sha256_digest(&session.csrf_digest) {
        return invalid("account session", &session.id, "session digest is invalid");
    }
    Ok(())
}

fn validate_account_id(id: &str) -> Result<(), RepositoryError> {
    if id == crate::auth::LOCAL_ACCOUNT_ID {
        return Ok(());
    }
    validate_uuid_id(id, "account:", "account")
}

fn validate_uuid_id(id: &str, prefix: &str, entity: &'static str) -> Result<(), RepositoryError> {
    let Some(value) = id.strip_prefix(prefix) else {
        return invalid(entity, id, "identity is invalid");
    };
    let valid = Uuid::parse_str(value).ok().is_some_and(|uuid| {
        uuid.get_version() == Some(Version::Random) && uuid.to_string() == value
    });
    if !valid {
        return invalid(entity, id, "identity is invalid");
    }
    Ok(())
}

fn validate_normalized_email(email: &str) -> Result<(), RepositoryError> {
    let valid = (3..=320).contains(&email.len())
        && email.trim() == email
        && email.to_lowercase() == email
        && !email.chars().any(char::is_whitespace)
        && email.split('@').filter(|part| !part.is_empty()).count() == 2
        && email.matches('@').count() == 1;
    if !valid {
        return invalid("account", "normalized-email", "normalized email is invalid");
    }
    Ok(())
}

fn validate_hmac_digest(value: &str) -> Result<(), RepositoryError> {
    let valid = value.len() == "hmac-sha256:".len() + 64
        && value.starts_with("hmac-sha256:")
        && value["hmac-sha256:".len()..]
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase());
    if !valid {
        return invalid(
            "authentication throttle",
            "key-digest",
            "HMAC digest is invalid",
        );
    }
    Ok(())
}

fn validate_bounded_opaque(
    value: &str,
    entity: &'static str,
    id: &str,
) -> Result<(), RepositoryError> {
    if !(1..=128).contains(&value.len())
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b':' | b'-'))
    {
        return invalid(entity, id, "opaque identifier is invalid");
    }
    Ok(())
}

fn to_positive_i64(value: u64, field: &'static str) -> Result<i64, RepositoryError> {
    let value = i64::try_from(value).map_err(|_| RepositoryError::NumericRange { field })?;
    if value <= 0 {
        return Err(RepositoryError::NumericRange { field });
    }
    Ok(value)
}

fn map_auth_insert_error(error: sqlx::Error, entity: &'static str, id: &str) -> RepositoryError {
    if error
        .as_database_error()
        .is_some_and(|database_error| database_error.is_unique_violation())
    {
        RepositoryError::AlreadyExists {
            entity,
            id: id.to_owned(),
        }
    } else {
        RepositoryError::Database(error)
    }
}

fn invalid<T>(entity: &'static str, id: &str, reason: &'static str) -> Result<T, RepositoryError> {
    Err(RepositoryError::InvalidDomainState {
        entity,
        id: id.to_owned(),
        reason,
    })
}

#[cfg(test)]
mod tests {
    use sqlx::{PgPool, Row};

    use super::*;
    use crate::repository::MIGRATOR;

    const PHC: &str =
        "$argon2id$v=19$m=19456,t=2,p=1$c29tZXNhbHQxMjM0NTY3OA$QWJjZGVmZ2hpamtsbW5vcHFyc3R1dnd4eXo";

    fn digest(character: char) -> String {
        format!("sha256:{}", character.to_string().repeat(64))
    }

    fn account(id: &str, email: &str) -> NewAccount {
        NewAccount {
            id: id.to_owned(),
            normalized_email: email.to_owned(),
            display_name: "Test Player".to_owned(),
            password_phc: PasswordPhc::parse(PHC).unwrap(),
        }
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn migration_creates_non_login_local_compatibility_account(pool: PgPool) {
        let row = sqlx::query(
            "SELECT normalized_email, password_phc, login_enabled
             FROM accounts WHERE id = 'account:local'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(!row.get::<bool, _>("login_enabled"));
        assert!(row.get::<Option<String>, _>("normalized_email").is_none());
        assert!(row.get::<Option<String>, _>("password_phc").is_none());
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn normalized_email_and_session_digests_are_unique(pool: PgPool) {
        let repository = PostgresRepository::from_pool(pool);
        let first_id = format!("account:{}", Uuid::new_v4());
        let second_id = format!("account:{}", Uuid::new_v4());
        repository
            .create_account(&account(&first_id, "player@example.test"))
            .await
            .unwrap();
        let duplicate = repository
            .create_account(&account(&second_id, "player@example.test"))
            .await
            .unwrap_err();
        assert!(matches!(duplicate, RepositoryError::AlreadyExists { .. }));

        let first_session = NewAccountSession {
            id: format!("session:{}", Uuid::new_v4()),
            account_id: first_id.clone(),
            token_digest: digest('a'),
            csrf_digest: digest('b'),
            idle_lifetime_seconds: 60,
            absolute_lifetime_seconds: 120,
        };
        repository
            .create_account_session(&first_session)
            .await
            .unwrap();
        let duplicate_token = NewAccountSession {
            id: format!("session:{}", Uuid::new_v4()),
            account_id: first_id,
            csrf_digest: digest('c'),
            ..first_session
        };
        assert!(matches!(
            repository
                .create_account_session(&duplicate_token)
                .await
                .unwrap_err(),
            RepositoryError::AlreadyExists { .. }
        ));
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn account_delete_cascades_sessions(pool: PgPool) {
        let repository = PostgresRepository::from_pool(pool.clone());
        let account_id = format!("account:{}", Uuid::new_v4());
        repository
            .create_account(&account(&account_id, "cascade@example.test"))
            .await
            .unwrap();
        repository
            .create_account_session(&NewAccountSession {
                id: format!("session:{}", Uuid::new_v4()),
                account_id: account_id.clone(),
                token_digest: digest('d'),
                csrf_digest: digest('e'),
                idle_lifetime_seconds: 60,
                absolute_lifetime_seconds: 120,
            })
            .await
            .unwrap();
        sqlx::query("DELETE FROM accounts WHERE id = $1")
            .bind(&account_id)
            .execute(&pool)
            .await
            .unwrap();
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM account_sessions")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 0);
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn expired_and_revoked_sessions_do_not_authenticate(pool: PgPool) {
        let repository = PostgresRepository::from_pool(pool.clone());
        let account_id = format!("account:{}", Uuid::new_v4());
        repository
            .create_account(&account(&account_id, "expiry@example.test"))
            .await
            .unwrap();
        let session_id = format!("session:{}", Uuid::new_v4());
        let token_digest = digest('f');
        repository
            .create_account_session(&NewAccountSession {
                id: session_id.clone(),
                account_id: account_id.clone(),
                token_digest: token_digest.clone(),
                csrf_digest: digest('0'),
                idle_lifetime_seconds: 60,
                absolute_lifetime_seconds: 120,
            })
            .await
            .unwrap();
        assert!(
            repository
                .authenticate_session_digest(&token_digest, 60)
                .await
                .unwrap()
                .is_some()
        );
        assert!(
            repository
                .revoke_account_session(&account_id, &session_id)
                .await
                .unwrap()
        );
        assert!(
            repository
                .authenticate_session_digest(&token_digest, 60)
                .await
                .unwrap()
                .is_none()
        );

        sqlx::query(
            "UPDATE account_sessions
             SET created_at = CURRENT_TIMESTAMP - INTERVAL '1 hour',
                 last_seen_at = CURRENT_TIMESTAMP - INTERVAL '30 minutes',
                 revoked_at = NULL,
                 idle_expires_at = CURRENT_TIMESTAMP - INTERVAL '2 seconds',
                 absolute_expires_at = CURRENT_TIMESTAMP - INTERVAL '1 second'
             WHERE id = $1",
        )
        .bind(&session_id)
        .execute(&pool)
        .await
        .unwrap();
        assert!(
            repository
                .authenticate_session_digest(&token_digest, 60)
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(
            repository.cleanup_expired_account_sessions().await.unwrap(),
            1
        );
    }
}
