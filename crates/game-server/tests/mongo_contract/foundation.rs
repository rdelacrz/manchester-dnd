use std::time::Duration;

use manchester_dnd_server::{
    CollectionName, MongoConfig, MongoFailureKind, MongoSchemaPolicy, MongoStore, PersistenceError,
    SCHEMA_BUNDLE_VERSION, SchemaReconciler, SecretString, collection_catalog,
};
use mongodb::bson::{DateTime, doc};
use uuid::Uuid;

#[test]
fn catalog_contract_names_every_planned_collection() {
    let catalog = collection_catalog();
    assert_eq!(catalog.len(), 34);
    assert!(catalog.iter().all(|entry| !entry.validator.is_empty()));
    assert_eq!(SCHEMA_BUNDLE_VERSION, 1);
}

#[tokio::test]
async fn replica_set_schema_and_transaction_contract() {
    let Ok(uri) = std::env::var("MONGODB_TEST_URI") else {
        eprintln!("skipping real MongoDB contract: MONGODB_TEST_URI is not set");
        return;
    };
    assert!(
        !uri.trim().is_empty(),
        "MONGODB_TEST_URI must not be empty when set"
    );
    let run_id = Uuid::new_v4().simple().to_string();
    let database = format!("mdnd_test_{run_id}");
    let config = MongoConfig {
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
    };
    let store = MongoStore::connect(&config).await.unwrap();
    let hello = store
        .database()
        .run_command(doc! { "hello": 1 })
        .await
        .unwrap();
    assert_eq!(
        hello.get_str("setName"),
        Ok("rs0"),
        "contract requires a replica set"
    );
    assert_eq!(
        hello.get_bool("isWritablePrimary"),
        Ok(true),
        "contract requires a primary"
    );

    let schema = SchemaReconciler::new(store.clone());
    let first = schema.apply().await.unwrap();
    assert_eq!(first.created_collections, 34);
    let second = schema.apply().await.unwrap();
    assert_eq!(second.created_collections, 0);
    assert_eq!(second.updated_validators, 0);
    assert_eq!(second.created_indexes, 0);
    assert!(!second.metadata_updated);
    let verified = schema.verify().await.unwrap();
    assert_eq!(verified.collections, 34);

    store
        .database()
        .run_command(doc! {
            "collMod": CollectionName::AuditEvents.as_str(),
            "validator": {},
            "validationLevel": "strict",
            "validationAction": "error",
        })
        .await
        .unwrap();
    assert!(schema.verify().await.is_err());
    assert_eq!(schema.apply().await.unwrap().updated_validators, 1);

    store
        .database()
        .run_command(doc! {
            "dropIndexes": CollectionName::AuditEvents.as_str(),
            "index": "mdnd_audit_events_category_created",
        })
        .await
        .unwrap();
    store
        .database()
        .run_command(doc! {
            "createIndexes": CollectionName::AuditEvents.as_str(),
            "indexes": [{
                "name": "mdnd_audit_events_category_created",
                "key": { "wrong_field": 1 },
            }],
        })
        .await
        .unwrap();
    assert!(schema.verify_indexes().await.is_err());
    store
        .database()
        .run_command(doc! {
            "dropIndexes": CollectionName::AuditEvents.as_str(),
            "index": "mdnd_audit_events_category_created",
        })
        .await
        .unwrap();
    assert_eq!(schema.apply().await.unwrap().created_indexes, 1);

    let audit_events = store.document_collection(CollectionName::AuditEvents);
    let invalid = audit_events
        .insert_one(doc! { "_id": format!("audit:invalid-{run_id}"), "schema_version": 1_i64 })
        .await
        .unwrap_err();
    assert_eq!(
        PersistenceError::mongo("invalid document contract", invalid).mongo_failure_kind(),
        Some(MongoFailureKind::DocumentValidation)
    );
    let event_id = format!("audit:{run_id}");
    let run_id_for_event = run_id.clone();
    store
        .with_transaction(move |session| {
            let audit_events = audit_events.clone();
            let event_id = event_id.clone();
            let scope_id = run_id_for_event.clone();
            Box::pin(async move {
                audit_events
                    .insert_one(doc! {
                        "_id": event_id,
                        "schema_version": 1_i64,
                        "category": "schema_contract",
                        "action": "transaction_probe",
                        "outcome": "committed",
                        "scope_kind": "test_run",
                        "scope_id": scope_id,
                        "created_at": DateTime::now(),
                    })
                    .session(session)
                    .await
                    .map_err(|error| {
                        PersistenceError::mongo("transaction contract insert", error)
                    })?;
                Ok(())
            })
        })
        .await
        .unwrap();

    assert!(
        database.starts_with("mdnd_test_") && database.ends_with(&run_id),
        "cleanup safeguard"
    );
    store.database().drop().await.unwrap();
}
