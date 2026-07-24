use std::collections::{BTreeMap, HashMap, HashSet};

use mongodb::bson::{Bson, DateTime, Document, doc};
use sha2::{Digest, Sha256};

use crate::error::PersistenceError;

use super::{
    CollectionName, MongoStore,
    indexes::{IndexSpec, MANAGED_INDEX_PREFIX, indexes_for},
    validators::validator_for,
};

pub const SCHEMA_BUNDLE_VERSION: i64 = 1;

#[derive(Debug, Clone, PartialEq)]
pub struct SchemaCatalogEntry {
    pub collection: CollectionName,
    pub validator: Document,
    pub indexes: Vec<IndexSpec>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SchemaApplyReport {
    pub created_collections: usize,
    pub updated_validators: usize,
    pub created_indexes: usize,
    pub metadata_updated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaVerificationReport {
    pub collections: usize,
    pub indexes: usize,
    pub bundle_version: i64,
    pub bundle_digest: String,
}

#[derive(Clone)]
pub struct SchemaReconciler {
    store: MongoStore,
}

impl SchemaReconciler {
    pub fn new(store: MongoStore) -> Self {
        Self { store }
    }

    pub async fn apply(&self) -> Result<SchemaApplyReport, PersistenceError> {
        let catalog = collection_catalog();
        let mut report = SchemaApplyReport::default();
        let mut existing = self.collection_options().await?;

        for entry in &catalog {
            let name = entry.collection.as_str();
            match existing.remove(name) {
                None => {
                    self.store
                        .database()
                        .run_command(doc! {
                            "create": name,
                            "validator": entry.validator.clone(),
                            "validationLevel": "strict",
                            "validationAction": "error",
                        })
                        .await
                        .map_err(|error| PersistenceError::mongo("create collection", error))?;
                    report.created_collections += 1;
                }
                Some(options) => {
                    let validator_matches = options
                        .get("validator")
                        .and_then(Bson::as_document)
                        .is_some_and(|actual| documents_equivalent(actual, &entry.validator));
                    let strict = options.get_str("validationLevel") == Ok("strict");
                    let errors = options.get_str("validationAction") == Ok("error");
                    if !validator_matches || !strict || !errors {
                        self.store
                            .database()
                            .run_command(doc! {
                                "collMod": name,
                                "validator": entry.validator.clone(),
                                "validationLevel": "strict",
                                "validationAction": "error",
                            })
                            .await
                            .map_err(|error| {
                                PersistenceError::mongo("update collection validator", error)
                            })?;
                        report.updated_validators += 1;
                    }
                }
            }
        }

        for entry in &catalog {
            report.created_indexes += self.apply_indexes(entry).await?;
        }
        report.metadata_updated = self.write_bundle_metadata().await?;
        self.verify().await?;
        Ok(report)
    }

    pub async fn verify(&self) -> Result<SchemaVerificationReport, PersistenceError> {
        let catalog = collection_catalog();
        let mut existing = self.collection_options().await?;
        for entry in &catalog {
            let name = entry.collection.as_str();
            let options = existing
                .remove(name)
                .ok_or_else(|| PersistenceError::SchemaDrift {
                    collection: name.to_owned(),
                    detail: "collection is missing".to_owned(),
                })?;
            if !options
                .get("validator")
                .and_then(Bson::as_document)
                .is_some_and(|actual| documents_equivalent(actual, &entry.validator))
                || options.get_str("validationLevel") != Ok("strict")
                || options.get_str("validationAction") != Ok("error")
            {
                return Err(PersistenceError::SchemaDrift {
                    collection: name.to_owned(),
                    detail: "validator or validation policy differs from the bundle".to_owned(),
                });
            }
        }

        let index_count = self.verify_indexes_for_catalog(&catalog).await?;
        let digest = schema_bundle_digest()?;
        let settings = self
            .store
            .document_collection(CollectionName::SystemSettings)
            .find_one(doc! { "_id": "system:settings" })
            .await
            .map_err(|error| PersistenceError::mongo("read schema bundle metadata", error))?
            .ok_or(PersistenceError::SchemaBundleMismatch)?;
        let bundle = settings
            .get_document("schema_bundle")
            .map_err(|_| PersistenceError::SchemaBundleMismatch)?;
        if bundle.get_i64("version") != Ok(SCHEMA_BUNDLE_VERSION)
            || bundle.get_str("digest") != Ok(digest.as_str())
        {
            return Err(PersistenceError::SchemaBundleMismatch);
        }

        Ok(SchemaVerificationReport {
            collections: catalog.len(),
            indexes: index_count,
            bundle_version: SCHEMA_BUNDLE_VERSION,
            bundle_digest: digest,
        })
    }

    pub async fn verify_indexes(&self) -> Result<usize, PersistenceError> {
        self.verify_indexes_for_catalog(&collection_catalog()).await
    }

    async fn collection_options(&self) -> Result<HashMap<String, Document>, PersistenceError> {
        let response = self
            .store
            .database()
            .run_command(doc! {
                "listCollections": 1,
                "nameOnly": false,
                "cursor": { "batchSize": 1000 },
            })
            .await
            .map_err(|error| PersistenceError::mongo("list collections", error))?;
        let mut output = HashMap::new();
        for entry in first_batch(&response, "list collections")? {
            let name = entry
                .get_str("name")
                .map_err(|_| PersistenceError::SchemaDrift {
                    collection: "<catalog>".to_owned(),
                    detail: "listCollections returned an entry without a name".to_owned(),
                })?;
            output.insert(
                name.to_owned(),
                entry
                    .get_document("options")
                    .cloned()
                    .unwrap_or_else(|_| Document::new()),
            );
        }
        Ok(output)
    }

    async fn list_indexes(&self, collection: &str) -> Result<Vec<Document>, PersistenceError> {
        let response = self
            .store
            .database()
            .run_command(doc! {
                "listIndexes": collection,
                "cursor": { "batchSize": 1000 },
            })
            .await
            .map_err(|error| PersistenceError::mongo("list indexes", error))?;
        first_batch(&response, "list indexes")
    }

    async fn apply_indexes(&self, entry: &SchemaCatalogEntry) -> Result<usize, PersistenceError> {
        let collection = entry.collection.as_str();
        let existing = self.list_indexes(collection).await?;
        detect_obsolete_managed_indexes(collection, &existing, &entry.indexes)?;
        let by_name = indexes_by_name(collection, &existing)?;
        let mut created = 0;
        for expected in &entry.indexes {
            if let Some(actual) = by_name.get(expected.name) {
                if !index_matches(actual, expected) {
                    return Err(PersistenceError::SchemaDrift {
                        collection: collection.to_owned(),
                        detail: format!(
                            "managed index {} has conflicting keys/options",
                            expected.name
                        ),
                    });
                }
                continue;
            }
            self.store
                .database()
                .run_command(doc! {
                    "createIndexes": collection,
                    "indexes": [expected.command_document()],
                })
                .await
                .map_err(|error| PersistenceError::mongo("create index", error))?;
            created += 1;
        }
        Ok(created)
    }

    async fn verify_indexes_for_catalog(
        &self,
        catalog: &[SchemaCatalogEntry],
    ) -> Result<usize, PersistenceError> {
        let mut count = 0;
        for entry in catalog {
            let collection = entry.collection.as_str();
            let existing = self.list_indexes(collection).await?;
            detect_obsolete_managed_indexes(collection, &existing, &entry.indexes)?;
            let by_name = indexes_by_name(collection, &existing)?;
            for expected in &entry.indexes {
                let actual =
                    by_name
                        .get(expected.name)
                        .ok_or_else(|| PersistenceError::SchemaDrift {
                            collection: collection.to_owned(),
                            detail: format!("managed index {} is missing", expected.name),
                        })?;
                if !index_matches(actual, expected) {
                    return Err(PersistenceError::SchemaDrift {
                        collection: collection.to_owned(),
                        detail: format!(
                            "managed index {} has conflicting keys/options",
                            expected.name
                        ),
                    });
                }
                count += 1;
            }
        }
        Ok(count)
    }

    async fn write_bundle_metadata(&self) -> Result<bool, PersistenceError> {
        let digest = schema_bundle_digest()?;
        let collection = self.document_collection();
        let existing = collection
            .find_one(doc! { "_id": "system:settings" })
            .await
            .map_err(|error| PersistenceError::mongo("read schema bundle metadata", error))?;
        let current = existing.as_ref().and_then(|settings| {
            let bundle = settings.get_document("schema_bundle").ok()?;
            Some((
                bundle.get_i64("version").ok()?,
                bundle.get_str("digest").ok()?,
            ))
        });
        if current == Some((SCHEMA_BUNDLE_VERSION, digest.as_str())) {
            return Ok(false);
        }

        let now = DateTime::now();
        if existing.is_none() {
            collection
                .insert_one(doc! {
                    "_id": "system:settings",
                    "schema_version": 1_i64,
                    "revision": 1_i64,
                    "schema_bundle": {
                        "version": SCHEMA_BUNDLE_VERSION,
                        "digest": &digest,
                        "applied_at": now,
                    },
                    "updated_at": now,
                })
                .await
                .map_err(|error| PersistenceError::mongo("insert schema bundle metadata", error))?;
        } else {
            collection
                .update_one(
                    doc! { "_id": "system:settings" },
                    doc! {
                        "$set": {
                            "schema_bundle": {
                                "version": SCHEMA_BUNDLE_VERSION,
                                "digest": &digest,
                                "applied_at": now,
                            },
                            "updated_at": now,
                        },
                        "$inc": { "revision": 1_i64 },
                    },
                )
                .await
                .map_err(|error| PersistenceError::mongo("update schema bundle metadata", error))?;
        }
        Ok(true)
    }

    fn document_collection(&self) -> mongodb::Collection<Document> {
        self.store
            .document_collection(CollectionName::SystemSettings)
    }
}

pub fn collection_catalog() -> Vec<SchemaCatalogEntry> {
    CollectionName::ALL
        .into_iter()
        .map(|collection| SchemaCatalogEntry {
            collection,
            validator: validator_for(collection),
            indexes: indexes_for(collection),
        })
        .collect()
}

pub fn schema_bundle_digest() -> Result<String, PersistenceError> {
    let mut hasher = Sha256::new();
    for entry in collection_catalog() {
        hash_field(&mut hasher, entry.collection.as_str().as_bytes());
        let validator = mongodb::bson::to_vec(&canonicalize_document(&entry.validator))
            .map_err(PersistenceError::BsonEncoding)?;
        hash_field(&mut hasher, &validator);
        for index in entry.indexes {
            let encoded = mongodb::bson::to_vec(&canonicalize_document(&index.command_document()))
                .map_err(PersistenceError::BsonEncoding)?;
            hash_field(&mut hasher, &encoded);
        }
    }
    Ok(format!("sha256:{:x}", hasher.finalize()))
}

fn hash_field(hasher: &mut Sha256, value: &[u8]) {
    hasher.update(u64::try_from(value.len()).unwrap_or(u64::MAX).to_le_bytes());
    hasher.update(value);
}

fn first_batch(response: &Document, operation: &str) -> Result<Vec<Document>, PersistenceError> {
    response
        .get_document("cursor")
        .and_then(|cursor| cursor.get_array("firstBatch"))
        .map_err(|_| PersistenceError::SchemaDrift {
            collection: "<catalog>".to_owned(),
            detail: format!("{operation} returned a malformed cursor"),
        })?
        .iter()
        .map(|value| {
            value
                .as_document()
                .cloned()
                .ok_or_else(|| PersistenceError::SchemaDrift {
                    collection: "<catalog>".to_owned(),
                    detail: format!("{operation} returned a non-document entry"),
                })
        })
        .collect()
}

fn detect_obsolete_managed_indexes(
    collection: &str,
    actual: &[Document],
    expected: &[IndexSpec],
) -> Result<(), PersistenceError> {
    let expected_names = expected
        .iter()
        .map(|index| index.name)
        .collect::<HashSet<_>>();
    for index in actual {
        let Ok(name) = index.get_str("name") else {
            continue;
        };
        if name.starts_with(MANAGED_INDEX_PREFIX) && !expected_names.contains(name) {
            return Err(PersistenceError::SchemaDrift {
                collection: collection.to_owned(),
                detail: format!("obsolete managed index {name} must be reviewed explicitly"),
            });
        }
    }
    Ok(())
}

fn indexes_by_name<'a>(
    collection: &str,
    indexes: &'a [Document],
) -> Result<HashMap<String, &'a Document>, PersistenceError> {
    indexes
        .iter()
        .map(|index| {
            let name = index
                .get_str("name")
                .map_err(|_| PersistenceError::SchemaDrift {
                    collection: collection.to_owned(),
                    detail: "listIndexes returned an entry without a string name".to_owned(),
                })?;
            Ok((name.to_owned(), index))
        })
        .collect()
}

fn index_matches(actual: &Document, expected: &IndexSpec) -> bool {
    actual
        .get_document("key")
        .is_ok_and(|keys| documents_equivalent(keys, &expected.keys))
        && actual.get_bool("unique").unwrap_or(false) == expected.unique
        && match (
            &expected.partial_filter,
            actual.get("partialFilterExpression"),
        ) {
            (None, None) => true,
            (Some(expected), Some(actual)) => actual
                .as_document()
                .is_some_and(|actual| documents_equivalent(actual, expected)),
            _ => false,
        }
        && match (
            expected.expire_after_seconds,
            actual.get("expireAfterSeconds"),
        ) {
            (None, None) => true,
            (Some(expected), Some(actual)) => integer_value(actual) == Some(expected),
            _ => false,
        }
}

fn documents_equivalent(left: &Document, right: &Document) -> bool {
    canonicalize_document(left) == canonicalize_document(right)
}

fn canonicalize_document(document: &Document) -> Document {
    canonicalize_bson(Bson::Document(document.clone()))
        .as_document()
        .cloned()
        .expect("document canonicalization preserves BSON type")
}

fn canonicalize_bson(value: Bson) -> Bson {
    match value {
        Bson::Document(document) => {
            let sorted = document
                .into_iter()
                .map(|(key, value)| (key, canonicalize_bson(value)))
                .collect::<BTreeMap<_, _>>();
            Bson::Document(sorted.into_iter().collect())
        }
        Bson::Array(values) => Bson::Array(values.into_iter().map(canonicalize_bson).collect()),
        Bson::Int32(value) => Bson::Int64(i64::from(value)),
        value => value,
    }
}

fn integer_value(value: &Bson) -> Option<i64> {
    match value {
        Bson::Int32(value) => Some(i64::from(*value)),
        Bson::Int64(value) => Some(*value),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_represents_all_34_planned_collections_once() {
        let catalog = collection_catalog();
        assert_eq!(catalog.len(), 34);
        let names = catalog
            .iter()
            .map(|entry| entry.collection.as_str())
            .collect::<HashSet<_>>();
        assert_eq!(names.len(), 34);
        assert!(
            catalog
                .iter()
                .all(|entry| entry.validator.contains_key("$jsonSchema"))
        );
    }

    #[test]
    fn bundle_digest_is_stable_and_credential_free() {
        let first = schema_bundle_digest().unwrap();
        let second = schema_bundle_digest().unwrap();
        assert_eq!(first, second);
        assert_eq!(first.len(), "sha256:".len() + 64);
    }

    #[test]
    fn index_comparison_normalizes_bson_integer_width() {
        let expected = IndexSpec {
            name: "mdnd_test",
            keys: doc! { "purge_at": 1 },
            unique: false,
            partial_filter: None,
            expire_after_seconds: Some(0),
        };
        let actual = doc! {
            "name": "mdnd_test",
            "key": { "purge_at": 1_i64 },
            "expireAfterSeconds": 0_i32,
            "v": 2,
        };
        assert!(index_matches(&actual, &expected));
    }
}
