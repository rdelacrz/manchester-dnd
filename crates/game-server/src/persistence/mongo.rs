use mongodb::{
    Client, Collection, Database,
    bson::{Document, doc},
    options::ClientOptions,
};

use crate::{
    config::{MongoConfig, validate_mongodb_database_name},
    error::PersistenceError,
};

use super::CollectionName;

/// Cloneable MongoDB handle. MongoDB remains authoritative; callers must pass a
/// session explicitly for every operation inside `with_transaction`.
#[derive(Clone)]
pub struct MongoStore {
    client: Client,
    database: Database,
    operation_timeout: std::time::Duration,
    transaction_timeout: std::time::Duration,
    transaction_max_retries: u32,
}

impl MongoStore {
    pub async fn connect(config: &MongoConfig) -> Result<Self, PersistenceError> {
        validate_mongodb_database_name(&config.database)
            .map_err(|_| PersistenceError::InvalidDatabaseName)?;
        let mut options = ClientOptions::parse(config.uri.expose_secret())
            .await
            .map_err(|_| PersistenceError::InvalidMongoUri)?;
        options.app_name = Some("manchester-dnd-server".to_owned());
        options.max_pool_size = Some(config.max_pool_size);
        options.min_pool_size = Some(config.min_pool_size);
        options.connect_timeout = Some(config.connect_timeout);
        options.server_selection_timeout = Some(config.server_selection_timeout);

        let client = Client::with_options(options)
            .map_err(|error| PersistenceError::mongo("client construction", error))?;
        let store = Self {
            database: client.database(&config.database),
            client,
            operation_timeout: config.operation_timeout,
            transaction_timeout: config.transaction_timeout,
            transaction_max_retries: config.transaction_max_retries,
        };
        store.ping().await?;
        Ok(store)
    }

    pub async fn ping(&self) -> Result<(), PersistenceError> {
        tokio::time::timeout(
            self.operation_timeout,
            self.database.run_command(doc! { "ping": 1 }),
        )
        .await
        .map_err(|_| PersistenceError::OperationTimeout { operation: "ping" })?
        .map(|_| ())
        .map_err(|error| PersistenceError::mongo("ping", error))
    }

    pub fn collection<T: Send + Sync>(&self, name: CollectionName) -> Collection<T> {
        self.database.collection(name.as_str())
    }

    pub fn document_collection(&self, name: CollectionName) -> Collection<Document> {
        self.collection(name)
    }

    pub fn database(&self) -> &Database {
        &self.database
    }

    pub fn client(&self) -> &Client {
        &self.client
    }

    pub(crate) const fn transaction_timeout(&self) -> std::time::Duration {
        self.transaction_timeout
    }

    pub const fn operation_timeout(&self) -> std::time::Duration {
        self.operation_timeout
    }

    pub(crate) const fn transaction_max_retries(&self) -> u32 {
        self.transaction_max_retries
    }
}
