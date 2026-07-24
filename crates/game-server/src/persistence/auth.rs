use std::{future::IntoFuture, time::Duration};

use mongodb::{
    Collection,
    bson::{DateTime, Document, doc},
    options::ReturnDocument,
};
use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq;
use uuid::Uuid;

use crate::{
    auth::{
        AccountPrincipal, AccountSummary, AuthenticatedSession, AuthenticationActionKind,
        AuthenticationThrottleBucket, PasswordPhc,
    },
    error::PersistenceError,
};

use super::{CollectionName, MongoStore, email_crypto::EmailCiphertext};

const SCHEMA_VERSION: u32 = 1;
const RETENTION_GRACE: Duration = Duration::from_secs(30 * 24 * 60 * 60);

#[derive(Clone)]
pub struct MongoAccountRepository {
    store: MongoStore,
}

#[derive(Debug, Clone)]
pub(crate) struct NewSignupAccessToken {
    pub id: String,
    pub token_digest: String,
    pub allowed_role: String,
    pub issued_by: String,
    pub expires_at: DateTime,
    pub purge_at: DateTime,
}

#[derive(Debug, Clone)]
pub(crate) struct NewSignupSession {
    pub id: String,
    pub bearer_digest: String,
    pub csrf_digest: String,
    pub access_token_digest: String,
    pub expires_at: DateTime,
    pub purge_at: DateTime,
}

#[derive(Debug, Clone)]
pub(crate) struct NewMongoAccount {
    pub id: String,
    pub username: String,
    pub username_normalized: String,
    pub email_ciphertext: EmailCiphertext,
    pub email_lookup_hmac: String,
    pub password_phc: PasswordPhc,
}

#[derive(Debug, Clone)]
pub(crate) struct NewMongoAccountSession {
    pub id: String,
    pub account_id: String,
    pub bearer_digest: String,
    pub csrf_digest: String,
    pub password_role_version: u32,
    pub idle_expires_at: DateTime,
    pub absolute_expires_at: DateTime,
    pub purge_at: DateTime,
}

#[derive(Debug, Clone)]
pub(crate) struct MongoLoginAccount {
    pub id: String,
    pub username: String,
    pub password_phc: PasswordPhc,
    pub login_enabled: bool,
    pub password_role_version: u32,
    pub created_at: DateTime,
    pub updated_at: DateTime,
}

#[derive(Debug, Clone)]
pub(crate) struct MongoSessionRecord {
    pub authenticated: AuthenticatedSession,
    pub role: String,
    pub password_role_version: u32,
    pub last_seen_at: DateTime,
}

#[derive(Debug, Clone)]
pub(crate) struct SessionWriteOutcome {
    pub session: MongoSessionRecord,
    pub revoked_bearer_digests: Vec<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct CompleteSignupWrite {
    pub signup_bearer_digest: String,
    pub signup_csrf_digest: String,
    pub account: NewMongoAccount,
    pub account_session: NewMongoAccountSession,
}

#[derive(Debug, Clone)]
pub(crate) struct CompleteSignupOutcome {
    pub account: AccountSummary,
    pub session: MongoSessionRecord,
}

#[derive(Debug, Clone)]
pub(crate) struct PasswordUpdateOutcome {
    pub password_role_version: u32,
    pub revoked_bearer_digests: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AccountDocument {
    #[serde(rename = "_id")]
    id: String,
    schema_version: u32,
    revision: u64,
    role: String,
    username: String,
    username_normalized: String,
    email_ciphertext: EmailCiphertext,
    email_key_id: String,
    email_lookup_hmac: String,
    password_phc: String,
    password_role_version: u32,
    login_enabled: bool,
    password_changed_at: DateTime,
    created_at: DateTime,
    updated_at: DateTime,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SignupAccessTokenDocument {
    #[serde(rename = "_id")]
    id: String,
    schema_version: u32,
    token_digest: String,
    state: String,
    allowed_role: String,
    issued_by: String,
    issued_at: DateTime,
    expires_at: DateTime,
    purge_at: DateTime,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reserved_at: Option<DateTime>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reserved_signup_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    consumed_at: Option<DateTime>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    revoked_at: Option<DateTime>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SignupSessionDocument {
    #[serde(rename = "_id")]
    id: String,
    schema_version: u32,
    token_digest: String,
    csrf_digest: String,
    access_token_id: String,
    state: String,
    created_at: DateTime,
    expires_at: DateTime,
    purge_at: DateTime,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    completed_at: Option<DateTime>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AccountSessionDocument {
    #[serde(rename = "_id")]
    id: String,
    schema_version: u32,
    account_id: String,
    bearer_digest: String,
    csrf_digest: String,
    password_role_version: u32,
    created_at: DateTime,
    last_seen_at: DateTime,
    idle_expires_at: DateTime,
    absolute_expires_at: DateTime,
    purge_at: DateTime,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    revoked_at: Option<DateTime>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ThrottleDocument {
    #[serde(rename = "_id")]
    id: String,
    schema_version: u32,
    key_digest: String,
    action_kind: String,
    count: u32,
    window_started_at: DateTime,
    purge_at: DateTime,
    created_at: DateTime,
    updated_at: DateTime,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    blocked_until: Option<DateTime>,
}

impl MongoAccountRepository {
    pub fn new(store: MongoStore) -> Self {
        Self { store }
    }

    pub fn store(&self) -> &MongoStore {
        &self.store
    }

    pub(crate) async fn issue_signup_access_token(
        &self,
        token: NewSignupAccessToken,
    ) -> Result<(), PersistenceError> {
        let access_tokens = self.access_tokens();
        let audits = self.audits();
        self.store
            .with_transaction(move |session| {
                let access_tokens = access_tokens.clone();
                let audits = audits.clone();
                let token = token.clone();
                Box::pin(async move {
                    let now = DateTime::now();
                    let document = SignupAccessTokenDocument {
                        id: token.id.clone(),
                        schema_version: SCHEMA_VERSION,
                        token_digest: token.token_digest,
                        state: "active".to_owned(),
                        allowed_role: token.allowed_role.clone(),
                        issued_by: token.issued_by.clone(),
                        issued_at: now,
                        expires_at: token.expires_at,
                        purge_at: token.purge_at,
                        reserved_at: None,
                        reserved_signup_session_id: None,
                        consumed_at: None,
                        revoked_at: None,
                    };
                    access_tokens
                        .insert_one(document)
                        .session(&mut *session)
                        .await
                        .map_err(|error| {
                            PersistenceError::mongo("issue signup access token", error)
                        })?;
                    insert_audit(
                        &audits,
                        session,
                        AuditWrite {
                            action: "signup_access_token_issued",
                            outcome: "success",
                            scope_kind: "signup_access_token",
                            scope_id: &token.id,
                            actor_account_id: None,
                            metadata: doc! {
                                "allowed_role": token.allowed_role,
                                "issued_by": token.issued_by,
                            },
                        },
                    )
                    .await
                })
            })
            .await
    }

    pub(crate) async fn begin_signup(
        &self,
        signup: NewSignupSession,
    ) -> Result<(), PersistenceError> {
        let access_tokens = self.access_tokens();
        let signup_sessions = self.signup_sessions();
        let audits = self.audits();
        self.store
            .with_transaction(move |session| {
                let access_tokens = access_tokens.clone();
                let signup_sessions = signup_sessions.clone();
                let audits = audits.clone();
                let signup = signup.clone();
                Box::pin(async move {
                    let now = DateTime::now();
                    let access = access_tokens
                        .find_one_and_update(
                            doc! {
                                "token_digest": &signup.access_token_digest,
                                "state": "active",
                                "allowed_role": "user",
                                "expires_at": { "$gt": now },
                                "reserved_signup_session_id": { "$exists": false },
                            },
                            doc! {
                                "$set": {
                                    "reserved_at": now,
                                    "reserved_signup_session_id": &signup.id,
                                }
                            },
                        )
                        .return_document(ReturnDocument::After)
                        .session(&mut *session)
                        .await
                        .map_err(|error| {
                            PersistenceError::mongo("reserve signup access token", error)
                        })?
                        .ok_or_else(|| PersistenceError::NotFound {
                            entity: "signup access token",
                            id: "presented-token".to_owned(),
                        })?;
                    signup_sessions
                        .insert_one(SignupSessionDocument {
                            id: signup.id.clone(),
                            schema_version: SCHEMA_VERSION,
                            token_digest: signup.bearer_digest,
                            csrf_digest: signup.csrf_digest,
                            access_token_id: access.id.clone(),
                            state: "active".to_owned(),
                            created_at: now,
                            expires_at: signup.expires_at,
                            purge_at: signup.purge_at,
                            completed_at: None,
                        })
                        .session(&mut *session)
                        .await
                        .map_err(|error| PersistenceError::mongo("create signup session", error))?;
                    insert_audit(
                        &audits,
                        session,
                        AuditWrite {
                            action: "signup_access_token_redeemed",
                            outcome: "success",
                            scope_kind: "signup_access_token",
                            scope_id: &access.id,
                            actor_account_id: None,
                            metadata: doc! { "signup_session_id": &signup.id },
                        },
                    )
                    .await
                })
            })
            .await
    }

    pub(crate) async fn revoke_signup_access_token(
        &self,
        token_id: &str,
    ) -> Result<bool, PersistenceError> {
        let access_tokens = self.access_tokens();
        let audits = self.audits();
        let token_id = token_id.to_owned();
        self.store
            .with_transaction(move |session| {
                let access_tokens = access_tokens.clone();
                let audits = audits.clone();
                let token_id = token_id.clone();
                Box::pin(async move {
                    let now = DateTime::now();
                    let revoked = access_tokens
                        .find_one_and_update(
                            doc! { "_id": &token_id, "state": "active" },
                            doc! {
                                "$set": {
                                    "state": "revoked",
                                    "revoked_at": now,
                                }
                            },
                        )
                        .return_document(ReturnDocument::After)
                        .session(&mut *session)
                        .await
                        .map_err(|error| {
                            PersistenceError::mongo("revoke signup access token", error)
                        })?;
                    let Some(revoked) = revoked else {
                        return Ok(false);
                    };
                    insert_audit(
                        &audits,
                        session,
                        AuditWrite {
                            action: "signup_access_token_revoked",
                            outcome: "success",
                            scope_kind: "signup_access_token",
                            scope_id: &revoked.id,
                            actor_account_id: None,
                            metadata: doc! {},
                        },
                    )
                    .await?;
                    Ok(true)
                })
            })
            .await
    }

    pub(crate) async fn complete_signup(
        &self,
        write: CompleteSignupWrite,
    ) -> Result<CompleteSignupOutcome, PersistenceError> {
        let access_tokens = self.access_tokens();
        let signup_sessions = self.signup_sessions();
        let accounts = self.accounts();
        let account_sessions = self.account_sessions();
        let audits = self.audits();
        self.store
            .with_transaction(move |session| {
                let access_tokens = access_tokens.clone();
                let signup_sessions = signup_sessions.clone();
                let accounts = accounts.clone();
                let account_sessions = account_sessions.clone();
                let audits = audits.clone();
                let write = write.clone();
                Box::pin(async move {
                    let now = DateTime::now();
                    let signup = signup_sessions
                        .find_one(doc! {
                            "token_digest": &write.signup_bearer_digest,
                            "state": "active",
                            "expires_at": { "$gt": now },
                        })
                        .session(&mut *session)
                        .await
                        .map_err(|error| PersistenceError::mongo("load signup session", error))?
                        .filter(|stored| {
                            constant_time_digest_eq(&stored.csrf_digest, &write.signup_csrf_digest)
                        })
                        .ok_or_else(|| PersistenceError::NotFound {
                            entity: "signup session",
                            id: "presented-session".to_owned(),
                        })?;
                    let access = access_tokens
                        .find_one(doc! {
                            "_id": &signup.access_token_id,
                            "state": "active",
                            "allowed_role": "user",
                            "expires_at": { "$gt": now },
                            "reserved_signup_session_id": &signup.id,
                        })
                        .session(&mut *session)
                        .await
                        .map_err(|error| {
                            PersistenceError::mongo("load reserved signup access token", error)
                        })?
                        .ok_or_else(|| PersistenceError::NotFound {
                            entity: "signup access token",
                            id: signup.access_token_id.clone(),
                        })?;

                    let account = AccountDocument {
                        id: write.account.id.clone(),
                        schema_version: SCHEMA_VERSION,
                        revision: 1,
                        role: "user".to_owned(),
                        username: write.account.username.clone(),
                        username_normalized: write.account.username_normalized,
                        email_key_id: write.account.email_ciphertext.key_id.clone(),
                        email_ciphertext: write.account.email_ciphertext,
                        email_lookup_hmac: write.account.email_lookup_hmac,
                        password_phc: write.account.password_phc.expose_secret().to_owned(),
                        password_role_version: 1,
                        login_enabled: true,
                        password_changed_at: now,
                        created_at: now,
                        updated_at: now,
                    };
                    accounts
                        .insert_one(account.clone())
                        .session(&mut *session)
                        .await
                        .map_err(|error| PersistenceError::mongo("create account", error))?;
                    let account_session = account_session_document(&write.account_session, now);
                    account_sessions
                        .insert_one(account_session.clone())
                        .session(&mut *session)
                        .await
                        .map_err(|error| {
                            PersistenceError::mongo("create initial account session", error)
                        })?;
                    let signup_update = signup_sessions
                        .update_one(
                            doc! {
                                "_id": &signup.id,
                                "state": "active",
                                "expires_at": { "$gt": now },
                            },
                            doc! {
                                "$set": {
                                    "state": "completed",
                                    "completed_at": now,
                                }
                            },
                        )
                        .session(&mut *session)
                        .await
                        .map_err(|error| {
                            PersistenceError::mongo("complete signup session", error)
                        })?;
                    let access_update = access_tokens
                        .update_one(
                            doc! {
                                "_id": &access.id,
                                "state": "active",
                                "reserved_signup_session_id": &signup.id,
                                "expires_at": { "$gt": now },
                            },
                            doc! {
                                "$set": {
                                    "state": "consumed",
                                    "consumed_at": now,
                                }
                            },
                        )
                        .session(&mut *session)
                        .await
                        .map_err(|error| {
                            PersistenceError::mongo("consume signup access token", error)
                        })?;
                    if signup_update.modified_count != 1 || access_update.modified_count != 1 {
                        return Err(PersistenceError::RevisionConflict {
                            entity: "signup",
                            id: signup.id,
                            expected: 1,
                            actual: 0,
                        });
                    }
                    insert_audit(
                        &audits,
                        session,
                        AuditWrite {
                            action: "account_created",
                            outcome: "success",
                            scope_kind: "account",
                            scope_id: &account.id,
                            actor_account_id: Some(&account.id),
                            metadata: doc! {
                                "role": "user",
                                "signup_access_token_id": access.id,
                            },
                        },
                    )
                    .await?;
                    Ok(CompleteSignupOutcome {
                        account: account_summary(&account)?,
                        session: session_record(&account_session, &account)?,
                    })
                })
            })
            .await
    }

    pub(crate) async fn load_login_account(
        &self,
        username_normalized: Option<&str>,
        email_lookup_hmac: Option<&str>,
    ) -> Result<Option<MongoLoginAccount>, PersistenceError> {
        let filter = match (username_normalized, email_lookup_hmac) {
            (Some(username), None) => doc! { "username_normalized": username },
            (None, Some(email_hmac)) => doc! { "email_lookup_hmac": email_hmac },
            _ => return Ok(None),
        };
        let found = self
            .operation("load login account", self.accounts().find_one(filter))
            .await?;
        found.map(login_account).transpose()
    }

    pub(crate) async fn create_login_session(
        &self,
        new_session: NewMongoAccountSession,
        maximum_active: u32,
    ) -> Result<SessionWriteOutcome, PersistenceError> {
        let accounts = self.accounts();
        let account_sessions = self.account_sessions();
        self.store
            .with_transaction(move |session| {
                let accounts = accounts.clone();
                let account_sessions = account_sessions.clone();
                let new_session = new_session.clone();
                Box::pin(async move {
                    let now = DateTime::now();
                    let account = accounts
                        .find_one(doc! {
                            "_id": &new_session.account_id,
                            "login_enabled": true,
                            "password_role_version": new_session.password_role_version,
                        })
                        .session(&mut *session)
                        .await
                        .map_err(|error| {
                            PersistenceError::mongo("load account for session", error)
                        })?
                        .ok_or_else(|| PersistenceError::NotFound {
                            entity: "account",
                            id: new_session.account_id.clone(),
                        })?;
                    let session_document = account_session_document(&new_session, now);
                    account_sessions
                        .insert_one(session_document.clone())
                        .session(&mut *session)
                        .await
                        .map_err(|error| {
                            PersistenceError::mongo("create account session", error)
                        })?;
                    let revoked = revoke_excess_sessions(
                        &account_sessions,
                        session,
                        &account.id,
                        &session_document.id,
                        maximum_active,
                        now,
                    )
                    .await?;
                    Ok(SessionWriteOutcome {
                        session: session_record(&session_document, &account)?,
                        revoked_bearer_digests: revoked,
                    })
                })
            })
            .await
    }

    pub(crate) async fn authenticate_session_digest(
        &self,
        bearer_digest: &str,
        idle_lifetime: Duration,
    ) -> Result<Option<MongoSessionRecord>, PersistenceError> {
        let accounts = self.accounts();
        let account_sessions = self.account_sessions();
        let bearer_digest = bearer_digest.to_owned();
        self.store
            .with_transaction(move |session| {
                let accounts = accounts.clone();
                let account_sessions = account_sessions.clone();
                let bearer_digest = bearer_digest.clone();
                Box::pin(async move {
                    let now = DateTime::now();
                    let Some(mut stored) = account_sessions
                        .find_one(doc! {
                            "bearer_digest": &bearer_digest,
                            "revoked_at": { "$exists": false },
                            "idle_expires_at": { "$gt": now },
                            "absolute_expires_at": { "$gt": now },
                        })
                        .session(&mut *session)
                        .await
                        .map_err(|error| PersistenceError::mongo("load account session", error))?
                    else {
                        return Ok(None);
                    };
                    let Some(account) = accounts
                        .find_one(doc! {
                            "_id": &stored.account_id,
                            "login_enabled": true,
                            "password_role_version": stored.password_role_version,
                        })
                        .session(&mut *session)
                        .await
                        .map_err(|error| {
                            PersistenceError::mongo("validate session account", error)
                        })?
                    else {
                        return Ok(None);
                    };
                    let idle_expires_at =
                        min_date(add_duration(now, idle_lifetime), stored.absolute_expires_at);
                    let update = account_sessions
                        .update_one(
                            doc! {
                                "_id": &stored.id,
                                "revoked_at": { "$exists": false },
                                "idle_expires_at": { "$gt": now },
                                "absolute_expires_at": { "$gt": now },
                            },
                            doc! {
                                "$set": {
                                    "last_seen_at": now,
                                    "idle_expires_at": idle_expires_at,
                                }
                            },
                        )
                        .session(&mut *session)
                        .await
                        .map_err(|error| {
                            PersistenceError::mongo("refresh account session", error)
                        })?;
                    if update.modified_count != 1 {
                        return Ok(None);
                    }
                    stored.last_seen_at = now;
                    stored.idle_expires_at = idle_expires_at;
                    Ok(Some(session_record(&stored, &account)?))
                })
            })
            .await
    }

    pub(crate) async fn update_password_and_revoke_sessions(
        &self,
        account_id: &str,
        password_phc: &PasswordPhc,
    ) -> Result<PasswordUpdateOutcome, PersistenceError> {
        let accounts = self.accounts();
        let account_sessions = self.account_sessions();
        let account_id = account_id.to_owned();
        let password_phc = password_phc.expose_secret().to_owned();
        self.store
            .with_transaction(move |session| {
                let accounts = accounts.clone();
                let account_sessions = account_sessions.clone();
                let account_id = account_id.clone();
                let password_phc = password_phc.clone();
                Box::pin(async move {
                    let now = DateTime::now();
                    let account = accounts
                        .find_one_and_update(
                            doc! { "_id": &account_id, "login_enabled": true },
                            doc! {
                                "$set": {
                                    "password_phc": password_phc,
                                    "password_changed_at": now,
                                    "updated_at": now,
                                },
                                "$inc": {
                                    "revision": 1_i64,
                                    "password_role_version": 1_i64,
                                }
                            },
                        )
                        .return_document(ReturnDocument::After)
                        .session(&mut *session)
                        .await
                        .map_err(|error| PersistenceError::mongo("update account password", error))?
                        .ok_or_else(|| PersistenceError::NotFound {
                            entity: "account",
                            id: account_id.clone(),
                        })?;
                    let revoked =
                        revoke_all_sessions(&account_sessions, session, &account_id, now).await?;
                    Ok(PasswordUpdateOutcome {
                        password_role_version: account.password_role_version,
                        revoked_bearer_digests: revoked,
                    })
                })
            })
            .await
    }

    pub(crate) async fn revoke_account_session(
        &self,
        account_id: &str,
        session_id: &str,
    ) -> Result<Option<String>, PersistenceError> {
        let now = DateTime::now();
        let found = self
            .operation(
                "revoke account session",
                self.account_sessions()
                    .find_one_and_update(
                        doc! {
                            "_id": session_id,
                            "account_id": account_id,
                            "revoked_at": { "$exists": false },
                        },
                        doc! { "$set": { "revoked_at": now } },
                    )
                    .return_document(ReturnDocument::After),
            )
            .await?;
        Ok(found.map(|session| session.bearer_digest))
    }

    pub(crate) async fn cleanup_expired_account_sessions(&self) -> Result<u64, PersistenceError> {
        let now = DateTime::now();
        let result = self
            .operation(
                "cleanup account sessions",
                self.account_sessions().delete_many(doc! {
                    "$or": [
                        { "idle_expires_at": { "$lte": now } },
                        { "absolute_expires_at": { "$lte": now } },
                        { "purge_at": { "$lte": now } },
                    ]
                }),
            )
            .await?;
        Ok(result.deleted_count)
    }

    pub(crate) async fn load_account_summary(
        &self,
        account_id: &str,
    ) -> Result<Option<AccountSummary>, PersistenceError> {
        let found = self
            .operation(
                "load account summary",
                self.accounts()
                    .find_one(doc! { "_id": account_id, "login_enabled": true }),
            )
            .await?;
        found.as_ref().map(account_summary).transpose()
    }

    pub(crate) async fn account_version_is_current(
        &self,
        account_id: &str,
        password_role_version: u32,
    ) -> Result<bool, PersistenceError> {
        Ok(self
            .operation(
                "validate cached session account version",
                self.accounts().find_one(doc! {
                    "_id": account_id,
                    "login_enabled": true,
                    "password_role_version": password_role_version,
                }),
            )
            .await?
            .is_some())
    }

    pub(crate) async fn record_authentication_attempt(
        &self,
        key_digest: &str,
        action: AuthenticationActionKind,
        window: Duration,
        block_after_attempts: u32,
        block: Duration,
    ) -> Result<AuthenticationThrottleBucket, PersistenceError> {
        let throttles = self.throttles();
        let key_digest = key_digest.to_owned();
        let action_kind = action.as_str().to_owned();
        let new_id = format!(
            "auth-throttle:{}",
            Uuid::new_v5(
                &Uuid::NAMESPACE_OID,
                format!("{action_kind}\0{key_digest}").as_bytes(),
            )
        );
        self.store
            .with_transaction(move |session| {
                let throttles = throttles.clone();
                let key_digest = key_digest.clone();
                let action_kind = action_kind.clone();
                let new_id = new_id.clone();
                Box::pin(async move {
                    let now = DateTime::now();
                    let existing = throttles
                        .find_one(doc! {
                            "_id": &new_id,
                        })
                        .session(&mut *session)
                        .await
                        .map_err(|error| {
                            PersistenceError::mongo("load auth throttle bucket", error)
                        })?;
                    let window_cutoff = DateTime::from_millis(
                        now.timestamp_millis()
                            .saturating_sub(duration_millis(window)),
                    );
                    let (id, created_at, window_started_at, count, old_blocked_until) =
                        match existing {
                            Some(existing) if existing.window_started_at > window_cutoff => (
                                existing.id,
                                existing.created_at,
                                existing.window_started_at,
                                existing.count.saturating_add(1),
                                existing.blocked_until,
                            ),
                            Some(existing) => (existing.id, existing.created_at, now, 1, None),
                            None => (new_id, now, now, 1, None),
                        };
                    let blocked_until = if count >= block_after_attempts {
                        Some(add_duration(now, block))
                    } else {
                        old_blocked_until.filter(|blocked| *blocked > now)
                    };
                    let purge_at = add_duration(now, window.max(block).max(Duration::from_secs(1)));
                    let replacement = ThrottleDocument {
                        id: id.clone(),
                        schema_version: SCHEMA_VERSION,
                        key_digest: key_digest.clone(),
                        action_kind: action_kind.clone(),
                        count,
                        window_started_at,
                        purge_at,
                        created_at,
                        updated_at: now,
                        blocked_until,
                    };
                    throttles
                        .replace_one(
                            doc! {
                                "_id": &id,
                            },
                            replacement,
                        )
                        .upsert(true)
                        .session(&mut *session)
                        .await
                        .map_err(|error| {
                            PersistenceError::mongo("write auth throttle bucket", error)
                        })?;
                    Ok(AuthenticationThrottleBucket {
                        attempt_count: count,
                        blocked_until: blocked_until.map(date_string).transpose()?,
                    })
                })
            })
            .await
    }

    fn accounts(&self) -> Collection<AccountDocument> {
        self.store.collection(CollectionName::Accounts)
    }

    fn access_tokens(&self) -> Collection<SignupAccessTokenDocument> {
        self.store.collection(CollectionName::SignupAccessTokens)
    }

    fn signup_sessions(&self) -> Collection<SignupSessionDocument> {
        self.store.collection(CollectionName::SignupSessions)
    }

    fn account_sessions(&self) -> Collection<AccountSessionDocument> {
        self.store.collection(CollectionName::AccountSessions)
    }

    fn throttles(&self) -> Collection<ThrottleDocument> {
        self.store.collection(CollectionName::AuthThrottleBuckets)
    }

    fn audits(&self) -> Collection<Document> {
        self.store.collection(CollectionName::AuditEvents)
    }

    async fn operation<T>(
        &self,
        operation: &'static str,
        future: impl IntoFuture<Output = mongodb::error::Result<T>>,
    ) -> Result<T, PersistenceError> {
        tokio::time::timeout(self.store.operation_timeout(), future.into_future())
            .await
            .map_err(|_| PersistenceError::OperationTimeout { operation })?
            .map_err(|error| PersistenceError::mongo(operation, error))
    }
}

struct AuditWrite<'a> {
    action: &'a str,
    outcome: &'a str,
    scope_kind: &'a str,
    scope_id: &'a str,
    actor_account_id: Option<&'a str>,
    metadata: Document,
}

async fn insert_audit(
    audits: &Collection<Document>,
    session: &mut mongodb::ClientSession,
    write: AuditWrite<'_>,
) -> Result<(), PersistenceError> {
    let mut audit = doc! {
        "_id": format!("audit:{}", Uuid::new_v4()),
        "schema_version": SCHEMA_VERSION,
        "category": "authentication",
        "action": write.action,
        "outcome": write.outcome,
        "scope_kind": write.scope_kind,
        "scope_id": write.scope_id,
        "correlation_id": format!("correlation:{}", Uuid::new_v4()),
        "metadata": write.metadata,
        "created_at": DateTime::now(),
    };
    if let Some(actor) = write.actor_account_id {
        audit.insert("actor_account_id", actor);
    }
    audits
        .insert_one(audit)
        .session(session)
        .await
        .map_err(|error| PersistenceError::mongo("write authentication audit", error))?;
    Ok(())
}

async fn revoke_excess_sessions(
    sessions: &Collection<AccountSessionDocument>,
    client_session: &mut mongodb::ClientSession,
    account_id: &str,
    keep_session_id: &str,
    maximum_active: u32,
    now: DateTime,
) -> Result<Vec<String>, PersistenceError> {
    let mut cursor = sessions
        .find(doc! {
            "account_id": account_id,
            "_id": { "$ne": keep_session_id },
            "revoked_at": { "$exists": false },
            "idle_expires_at": { "$gt": now },
            "absolute_expires_at": { "$gt": now },
        })
        .sort(doc! { "created_at": -1, "_id": -1 })
        .skip(u64::from(maximum_active.saturating_sub(1)))
        .session(&mut *client_session)
        .await
        .map_err(|error| PersistenceError::mongo("find excess account sessions", error))?;
    let mut revoked = Vec::new();
    let mut ids = Vec::new();
    while cursor
        .advance(&mut *client_session)
        .await
        .map_err(|error| PersistenceError::mongo("read excess account sessions", error))?
    {
        let stored = cursor
            .deserialize_current()
            .map_err(|error| PersistenceError::mongo("decode excess account session", error))?;
        ids.push(stored.id);
        revoked.push(stored.bearer_digest);
    }
    if !ids.is_empty() {
        sessions
            .update_many(
                doc! { "_id": { "$in": ids } },
                doc! { "$set": { "revoked_at": now } },
            )
            .session(client_session)
            .await
            .map_err(|error| PersistenceError::mongo("revoke excess account sessions", error))?;
    }
    Ok(revoked)
}

async fn revoke_all_sessions(
    sessions: &Collection<AccountSessionDocument>,
    client_session: &mut mongodb::ClientSession,
    account_id: &str,
    now: DateTime,
) -> Result<Vec<String>, PersistenceError> {
    let mut cursor = sessions
        .find(doc! {
            "account_id": account_id,
            "revoked_at": { "$exists": false },
        })
        .session(&mut *client_session)
        .await
        .map_err(|error| PersistenceError::mongo("find account sessions to revoke", error))?;
    let mut revoked = Vec::new();
    while cursor
        .advance(&mut *client_session)
        .await
        .map_err(|error| PersistenceError::mongo("read account sessions to revoke", error))?
    {
        let stored = cursor
            .deserialize_current()
            .map_err(|error| PersistenceError::mongo("decode account session to revoke", error))?;
        revoked.push(stored.bearer_digest);
    }
    if !revoked.is_empty() {
        sessions
            .update_many(
                doc! {
                    "account_id": account_id,
                    "revoked_at": { "$exists": false },
                },
                doc! { "$set": { "revoked_at": now } },
            )
            .session(client_session)
            .await
            .map_err(|error| PersistenceError::mongo("revoke account sessions", error))?;
    }
    Ok(revoked)
}

fn account_session_document(
    session: &NewMongoAccountSession,
    created_at: DateTime,
) -> AccountSessionDocument {
    AccountSessionDocument {
        id: session.id.clone(),
        schema_version: SCHEMA_VERSION,
        account_id: session.account_id.clone(),
        bearer_digest: session.bearer_digest.clone(),
        csrf_digest: session.csrf_digest.clone(),
        password_role_version: session.password_role_version,
        created_at,
        last_seen_at: created_at,
        idle_expires_at: session.idle_expires_at,
        absolute_expires_at: session.absolute_expires_at,
        purge_at: session.purge_at,
        revoked_at: None,
    }
}

fn login_account(account: AccountDocument) -> Result<MongoLoginAccount, PersistenceError> {
    let password_phc =
        PasswordPhc::parse(account.password_phc).map_err(|_| PersistenceError::SchemaDrift {
            collection: CollectionName::Accounts.as_str().to_owned(),
            detail: "stored password verifier is invalid".to_owned(),
        })?;
    Ok(MongoLoginAccount {
        id: account.id,
        username: account.username,
        password_phc,
        login_enabled: account.login_enabled,
        password_role_version: account.password_role_version,
        created_at: account.created_at,
        updated_at: account.updated_at,
    })
}

fn account_summary(account: &AccountDocument) -> Result<AccountSummary, PersistenceError> {
    Ok(AccountSummary {
        id: account.id.clone(),
        display_name: account.username.clone(),
        login_enabled: account.login_enabled,
        created_at: date_string(account.created_at)?,
        updated_at: date_string(account.updated_at)?,
    })
}

fn session_record(
    session: &AccountSessionDocument,
    account: &AccountDocument,
) -> Result<MongoSessionRecord, PersistenceError> {
    Ok(MongoSessionRecord {
        authenticated: AuthenticatedSession {
            principal: AccountPrincipal {
                account_id: session.account_id.clone(),
                session_id: session.id.clone(),
            },
            csrf_digest: session.csrf_digest.clone(),
            idle_expires_at: date_string(session.idle_expires_at)?,
            absolute_expires_at: date_string(session.absolute_expires_at)?,
        },
        role: account.role.clone(),
        password_role_version: account.password_role_version,
        last_seen_at: session.last_seen_at,
    })
}

fn date_string(value: DateTime) -> Result<String, PersistenceError> {
    value
        .try_to_rfc3339_string()
        .map_err(|_| PersistenceError::SchemaDrift {
            collection: "authentication".to_owned(),
            detail: "stored BSON date is outside RFC 3339 range".to_owned(),
        })
}

pub(crate) fn add_duration(value: DateTime, duration: Duration) -> DateTime {
    DateTime::from_millis(
        value
            .timestamp_millis()
            .saturating_add(duration_millis(duration)),
    )
}

pub(crate) fn purge_after(value: DateTime) -> DateTime {
    add_duration(value, RETENTION_GRACE)
}

fn duration_millis(duration: Duration) -> i64 {
    i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
}

fn min_date(left: DateTime, right: DateTime) -> DateTime {
    if left <= right { left } else { right }
}

fn constant_time_digest_eq(left: &str, right: &str) -> bool {
    left.as_bytes().ct_eq(right.as_bytes()).into()
}
