use std::{future::Future, pin::Pin, time::Instant};

use mongodb::{
    ClientSession,
    options::{ReadConcern, ReadPreference, SelectionCriteria, WriteConcern},
};

use crate::error::{MongoFailureKind, PersistenceError};

use super::MongoStore;

pub type TransactionFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, PersistenceError>> + Send + 'a>>;

impl MongoStore {
    /// Runs an idempotent, database-only callback in a short transaction.
    ///
    /// Callback may be invoked more than once. It must not perform network
    /// calls, filesystem writes, publication, or any other external side effect.
    pub async fn with_transaction<T, F>(&self, mut callback: F) -> Result<T, PersistenceError>
    where
        T: Send,
        F: for<'session> FnMut(&'session mut ClientSession) -> TransactionFuture<'session, T>
            + Send,
    {
        let started = Instant::now();
        let max_attempts = self.transaction_max_retries().saturating_add(1);
        let mut attempt = 0;

        'transaction: loop {
            attempt += 1;
            let mut session = tokio::time::timeout(
                remaining(started, self.transaction_timeout())?,
                self.client().start_session(),
            )
            .await
            .map_err(|_| PersistenceError::TransactionDeadline)?
            .map_err(|error| PersistenceError::mongo("start session", error))?;

            let start_result = tokio::time::timeout(
                remaining(started, self.transaction_timeout())?,
                session
                    .start_transaction()
                    .read_concern(ReadConcern::snapshot())
                    .write_concern(WriteConcern::majority())
                    .selection_criteria(SelectionCriteria::ReadPreference(ReadPreference::Primary))
                    .max_commit_time(self.transaction_timeout()),
            )
            .await
            .map_err(|_| PersistenceError::TransactionDeadline)?
            .map_err(|error| PersistenceError::mongo("start transaction", error));
            if let Err(error) = start_result {
                if is_transient_transaction(&error) {
                    if attempt < max_attempts {
                        continue;
                    }
                    return Err(retries_exhausted(attempt, error));
                }
                return Err(error);
            }

            let value = match tokio::time::timeout(
                remaining(started, self.transaction_timeout())?,
                callback(&mut session),
            )
            .await
            {
                Err(_) => return Err(PersistenceError::TransactionDeadline),
                Ok(Ok(value)) => value,
                Ok(Err(error)) => {
                    tokio::time::timeout(
                        remaining(started, self.transaction_timeout())?,
                        session.abort_transaction(),
                    )
                    .await
                    .map_err(|_| PersistenceError::TransactionDeadline)?
                    .map_err(|abort| PersistenceError::mongo("abort transaction", abort))?;
                    if is_transient_transaction(&error) {
                        if attempt < max_attempts {
                            continue;
                        }
                        return Err(retries_exhausted(attempt, error));
                    }
                    return Err(error);
                }
            };

            let mut commit_attempt = 0;
            loop {
                commit_attempt += 1;
                let commit = tokio::time::timeout(
                    remaining(started, self.transaction_timeout())?,
                    session.commit_transaction(),
                )
                .await
                .map_err(|_| PersistenceError::TransactionDeadline)?
                .map_err(|error| PersistenceError::mongo("commit transaction", error));
                match commit {
                    Ok(()) => return Ok(value),
                    Err(error)
                        if error.mongo_failure_kind()
                            == Some(MongoFailureKind::UnknownCommitResult) =>
                    {
                        if commit_attempt < max_attempts {
                            continue;
                        }
                        return Err(retries_exhausted(commit_attempt, error));
                    }
                    Err(error) if is_transient_transaction(&error) => {
                        if attempt < max_attempts {
                            continue 'transaction;
                        }
                        return Err(retries_exhausted(attempt, error));
                    }
                    Err(error) => return Err(error),
                }
            }
        }
    }
}

fn remaining(
    started: Instant,
    transaction_timeout: std::time::Duration,
) -> Result<std::time::Duration, PersistenceError> {
    transaction_timeout
        .checked_sub(started.elapsed())
        .filter(|duration| !duration.is_zero())
        .ok_or(PersistenceError::TransactionDeadline)
}

fn is_transient_transaction(error: &PersistenceError) -> bool {
    error.mongo_failure_kind() == Some(MongoFailureKind::TransientTransaction)
}

fn retries_exhausted(attempts: u32, last: PersistenceError) -> PersistenceError {
    PersistenceError::TransactionRetriesExhausted {
        attempts,
        last: Box::new(last),
    }
}
