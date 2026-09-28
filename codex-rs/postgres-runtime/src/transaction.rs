//! Bounded PostgreSQL transaction outcomes without implicit retries.

use crate::PostgresPool;
use sqlx::PgConnection;
use sqlx::Postgres;
use sqlx::Transaction;
use std::fmt;
use std::time::Duration;
use tokio::time::timeout;

/// Redacted operation outcome. A conflict or deadlock is known to have aborted;
/// a lost COMMIT response may have committed and needs external reconciliation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransactionError {
    Timeout,
    SerializationConflict,
    Deadlock,
    Privilege,
    Rejected,
    Unavailable,
    CommitOutcomeUnknown,
}

impl fmt::Display for TransactionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PostgreSQL transaction error: {self:?}")
    }
}

impl std::error::Error for TransactionError {}

impl TransactionError {
    /// Map a statement failure before it crosses a logging or API boundary.
    pub fn classify_statement(error: &sqlx::Error) -> Self {
        match error {
            sqlx::Error::Database(error) => match error.code().as_deref() {
                Some("40001") => Self::SerializationConflict,
                Some("40P01") => Self::Deadlock,
                Some("42501") => Self::Privilege,
                _ => Self::Rejected,
            },
            sqlx::Error::PoolTimedOut => Self::Timeout,
            _ => Self::Unavailable,
        }
    }

    fn classify_commit(error: &sqlx::Error) -> Self {
        match error {
            sqlx::Error::Database(error) => Self::classify_commit_sqlstate(error.code().as_deref()),
            _ => Self::CommitOutcomeUnknown,
        }
    }

    fn classify_commit_sqlstate(code: Option<&str>) -> Self {
        match code {
            Some("40001") => Self::SerializationConflict,
            Some("40P01") => Self::Deadlock,
            Some("42501") => Self::Privilege,
            Some(code) if code.starts_with("08") || matches!(code, "57P01" | "57P02" | "57P03") => {
                Self::CommitOutcomeUnknown
            }
            Some(_) => Self::Rejected,
            None => Self::CommitOutcomeUnknown,
        }
    }
}

/// One owned SERIALIZABLE transaction. Dropping it schedules SQLx rollback.
/// Query futures and the connection may be cancelled; no operation is retried.
pub struct PostgresTransaction {
    inner: Transaction<'static, Postgres>,
    wait: Duration,
}

impl PostgresPool {
    /// Begin a SERIALIZABLE transaction within this pool's bounded acquire wait.
    pub async fn begin_serializable(&self) -> Result<PostgresTransaction, TransactionError> {
        let inner = timeout(
            self.acquire_timeout,
            self.pool.begin_with("BEGIN ISOLATION LEVEL SERIALIZABLE"),
        )
        .await
        .map_err(|_| TransactionError::Timeout)?
        .map_err(|error| TransactionError::classify_statement(&error))?;
        Ok(PostgresTransaction {
            inner,
            wait: self.acquire_timeout,
        })
    }
}

impl PostgresTransaction {
    /// Use this for parameterized SQLx statements and classify their errors.
    /// Do not issue raw transaction control SQL through this connection: use
    /// `commit` or `rollback` to preserve the outcome contract. Callers must
    /// bound their own statement waits and avoid logging raw SQLx errors.
    pub fn connection(&mut self) -> &mut PgConnection {
        &mut self.inner
    }

    /// Commit once. A timeout or transport failure during COMMIT is ambiguous,
    /// so callers must reconcile durable state rather than blindly retrying.
    pub async fn commit(self) -> Result<(), TransactionError> {
        timeout(self.wait, self.inner.commit())
            .await
            .map_err(|_| TransactionError::CommitOutcomeUnknown)?
            .map_err(|error| TransactionError::classify_commit(&error))
    }

    /// Explicitly roll back; dropping also schedules a rollback on cancellation.
    pub async fn rollback(self) -> Result<(), TransactionError> {
        timeout(self.wait, self.inner.rollback())
            .await
            .map_err(|_| TransactionError::Unavailable)?
            .map_err(|error| TransactionError::classify_statement(&error))
    }
}

#[cfg(test)]
#[path = "transaction_tests.rs"]
mod tests;
