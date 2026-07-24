use std::time::Duration;

use manchester_dnd_server::{
    AuthService, AuthenticationActionKind, AuthenticationConfig, AuthenticationError,
    AuthenticationSecret, CacheService, CollectionName, MongoAccountRepository, MongoConfig,
    MongoSchemaPolicy, MongoStore, SchemaReconciler, SecretString,
};
use mongodb::bson::{DateTime, Document, doc};
use uuid::Uuid;

#[tokio::test]
async fn mongo_authentication_vertical_slice_contract() {
    let Some((store, database)) = isolated_store().await else {
        return;
    };
    SchemaReconciler::new(store.clone()).apply().await.unwrap();
    let authentication = AuthenticationConfig {
        session_idle_lifetime: Duration::from_secs(60),
        session_absolute_lifetime: Duration::from_secs(600),
        max_active_sessions: 2,
        argon2_memory_kib: 8_192,
        argon2_iterations: 1,
        argon2_parallelism: 1,
        throttle_block_after_attempts: 3,
        throttle_window_seconds: 300,
        throttle_block_seconds: 60,
        ..Default::default()
    };
    let service = AuthService::new(
        MongoAccountRepository::new(store.clone()),
        CacheService::disabled(),
        authentication,
    )
    .unwrap();

    let issued_access = service
        .issue_signup_access_token("user", "operator:contract", Duration::from_secs(3_600))
        .await
        .unwrap();
    let raw_access = issued_access.token.expose_secret().to_owned();
    assert!(raw_access.len() >= 43, "token must carry at least 256 bits");
    let signup = service.begin_signup(&issued_access.token).await.unwrap();
    let raw_signup = signup.session_token.expose_secret().to_owned();
    let raw_signup_csrf = signup.csrf_token.expose_secret().to_owned();
    assert!(matches!(
        service
            .begin_signup(&AuthenticationSecret::new(raw_access.clone()))
            .await,
        Err(AuthenticationError::InvalidSignupAccess)
    ));

    let raw_password = "contract passphrase longer than fifteen";
    let issued_session = service
        .complete_signup(
            &signup.session_token,
            &signup.csrf_token,
            " Player@Example.Test ",
            "Contract Player",
            &AuthenticationSecret::new(raw_password),
        )
        .await
        .unwrap();
    let raw_session = issued_session.session_token.expose_secret().to_owned();
    let raw_session_csrf = issued_session.csrf_token.expose_secret().to_owned();
    assert_eq!(issued_session.account.display_name, "Contract Player");

    let login = service
        .login(
            "PLAYER@example.test",
            &AuthenticationSecret::new(raw_password),
        )
        .await
        .unwrap();
    let authenticated = service.authenticate(&login.session_token).await.unwrap();
    assert_eq!(
        authenticated.principal.account_id,
        issued_session.account.id
    );
    service.logout(&authenticated.principal).await.unwrap();
    assert!(matches!(
        service.authenticate(&login.session_token).await,
        Err(AuthenticationError::InvalidSession)
    ));

    let expiry_login = service
        .login("contract player", &AuthenticationSecret::new(raw_password))
        .await
        .unwrap();
    store
        .document_collection(CollectionName::AccountSessions)
        .update_one(
            doc! { "_id": &expiry_login.principal.session_id },
            doc! {
                "$set": {
                    "idle_expires_at": DateTime::from_millis(1),
                    "absolute_expires_at": DateTime::from_millis(1),
                }
            },
        )
        .await
        .unwrap();
    assert!(matches!(
        service.authenticate(&expiry_login.session_token).await,
        Err(AuthenticationError::InvalidSession)
    ));

    let expired_access = service
        .issue_signup_access_token("user", "operator:contract", Duration::from_secs(60))
        .await
        .unwrap();
    store
        .document_collection(CollectionName::SignupAccessTokens)
        .update_one(
            doc! { "_id": &expired_access.id },
            doc! { "$set": { "expires_at": DateTime::from_millis(1) } },
        )
        .await
        .unwrap();
    assert!(matches!(
        service.begin_signup(&expired_access.token).await,
        Err(AuthenticationError::InvalidSignupAccess)
    ));

    let revoked_access = service
        .issue_signup_access_token("user", "operator:contract", Duration::from_secs(60))
        .await
        .unwrap();
    service
        .revoke_signup_access_token(&revoked_access.id)
        .await
        .unwrap();
    assert!(matches!(
        service.begin_signup(&revoked_access.token).await,
        Err(AuthenticationError::InvalidSignupAccess)
    ));

    let expiring_signup_access = service
        .issue_signup_access_token("user", "operator:contract", Duration::from_secs(60))
        .await
        .unwrap();
    let expired_signup = service
        .begin_signup(&expiring_signup_access.token)
        .await
        .unwrap();
    store
        .document_collection(CollectionName::SignupSessions)
        .update_one(
            doc! { "_id": &expired_signup.id },
            doc! { "$set": { "expires_at": DateTime::from_millis(1) } },
        )
        .await
        .unwrap();
    assert!(matches!(
        service
            .complete_signup(
                &expired_signup.session_token,
                &expired_signup.csrf_token,
                "other@example.test",
                "Other Player",
                &AuthenticationSecret::new("another contract passphrase"),
            )
            .await,
        Err(AuthenticationError::InvalidSignupSession)
    ));

    let throttle_digest =
        service.throttle_key_digest("198.51.100.9", AuthenticationActionKind::SignUp);
    let mut throttle = None;
    for _ in 0..3 {
        throttle = Some(
            service
                .record_authentication_attempt(&throttle_digest, AuthenticationActionKind::SignUp)
                .await
                .unwrap(),
        );
    }
    assert!(AuthService::is_throttled(&throttle.unwrap()));
    let stored_throttle = store
        .document_collection(CollectionName::AuthThrottleBuckets)
        .find_one(doc! { "key_digest": &throttle_digest })
        .await
        .unwrap()
        .expect("disabled DragonflyDB must use MongoDB throttle fallback");
    assert!(!format!("{stored_throttle:?}").contains("198.51.100.9"));

    let account = store
        .document_collection(CollectionName::Accounts)
        .find_one(doc! { "_id": &issued_session.account.id })
        .await
        .unwrap()
        .unwrap();
    assert!(account.get_document("email_ciphertext").is_ok());
    assert!(
        account
            .get_str("email_lookup_hmac")
            .unwrap()
            .starts_with("hmac-sha256:")
    );
    assert!(
        account
            .get_str("password_phc")
            .unwrap()
            .starts_with("$argon2id$")
    );
    assert!(!account.contains_key("email"));
    assert!(!account.contains_key("normalized_email"));

    let forbidden = [
        raw_access.as_str(),
        raw_signup.as_str(),
        raw_signup_csrf.as_str(),
        raw_session.as_str(),
        raw_session_csrf.as_str(),
        raw_password,
        "player@example.test",
    ];
    for collection in [
        CollectionName::Accounts,
        CollectionName::SignupAccessTokens,
        CollectionName::SignupSessions,
        CollectionName::AccountSessions,
        CollectionName::AuthThrottleBuckets,
        CollectionName::AuditEvents,
    ] {
        let serialized = all_documents_debug(&store, collection).await;
        for plaintext in forbidden {
            assert!(
                !serialized.contains(plaintext),
                "{} leaked forbidden plaintext",
                collection.as_str()
            );
        }
    }

    assert!(
        database.starts_with("mdnd_auth_test_") && database != "manchester_dnd",
        "cleanup safeguard"
    );
    store.database().drop().await.unwrap();
}

async fn isolated_store() -> Option<(MongoStore, String)> {
    let Ok(uri) = std::env::var("MONGODB_TEST_URI") else {
        eprintln!("skipping MongoDB auth contract: MONGODB_TEST_URI is not set");
        return None;
    };
    assert!(
        uri.starts_with("mongodb://root:") && uri.contains("127.0.0.1"),
        "MONGODB_TEST_URI must be the explicit local root test URI"
    );
    let database = format!("mdnd_auth_test_{}", Uuid::new_v4().simple());
    let store = MongoStore::connect(&MongoConfig {
        uri: SecretString::new(uri),
        database: database.clone(),
        max_pool_size: 4,
        min_pool_size: 0,
        connect_timeout: Duration::from_secs(5),
        server_selection_timeout: Duration::from_secs(5),
        operation_timeout: Duration::from_secs(15),
        transaction_timeout: Duration::from_secs(10),
        transaction_max_retries: 2,
        schema_policy: MongoSchemaPolicy::ApplyAndVerify,
    })
    .await
    .unwrap();
    Some((store, database))
}

async fn all_documents_debug(store: &MongoStore, collection: CollectionName) -> String {
    let mut cursor = store
        .document_collection(collection)
        .find(doc! {})
        .await
        .unwrap();
    let mut output = String::new();
    while cursor.advance().await.unwrap() {
        let document: Document = cursor.deserialize_current().unwrap();
        output.push_str(&format!("{document:?}"));
    }
    output
}
