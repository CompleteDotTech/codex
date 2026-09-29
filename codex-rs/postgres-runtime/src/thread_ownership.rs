//! Inactive per-thread lease records for cooperating clients. No runtime write
//! path consumes these yet, and runtime credentials can directly modify rows.
//!
//! A future writer must validate ownership while holding the ownership row lock
//! in the same transaction as its data changes. An earlier `observe` call is not
//! a server-enforced fence. Old clients and local rollout files are outside
//! this primitive. Privileged enforcement belongs to a later stage.

use crate::PostgresPool;
use crate::TransactionError;
use std::fmt;
use std::time::Duration;
use tokio::time::timeout;

const MAX_LEASE: Duration = Duration::from_secs(30);

/// A durable claim returned after its PostgreSQL transaction commits.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ThreadOwnership {
    pub thread_id: String,
    pub owner_id: String,
    pub token: i64,
}

/// Redacted ownership error. An unknown commit outcome requires readback.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ThreadOwnershipError {
    InvalidLease,
    Transaction(TransactionError),
}

impl fmt::Display for ThreadOwnershipError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PostgreSQL thread ownership error: {self:?}")
    }
}

impl std::error::Error for ThreadOwnershipError {}

impl From<TransactionError> for ThreadOwnershipError {
    fn from(error: TransactionError) -> Self {
        Self::Transaction(error)
    }
}

fn lease_millis(duration: Duration) -> Result<i64, ThreadOwnershipError> {
    if duration.is_zero() || duration > MAX_LEASE {
        return Err(ThreadOwnershipError::InvalidLease);
    }
    Ok(duration.as_millis() as i64)
}

impl PostgresPool {
    /// Claim an absent or expired thread lease. `owner_id` must be a fresh UUID
    /// per claim attempt so an unknown COMMIT can be resolved by readback.
    /// Contention yields `None` or a serialization conflict.
    pub async fn claim_thread_ownership(
        &self,
        thread_id: &str,
        owner_id: &str,
        lease: Duration,
    ) -> Result<Option<ThreadOwnership>, ThreadOwnershipError> {
        let millis = lease_millis(lease)?;
        let mut transaction = self.begin_serializable().await?;
        timeout(
            self.acquire_timeout,
            sqlx::query("INSERT INTO codex_storage.thread_writer_ownership (thread_id, token) VALUES ($1::uuid, 0) ON CONFLICT (thread_id) DO NOTHING")
                .bind(thread_id)
                .execute(transaction.connection()),
        )
        .await
        .map_err(|_| TransactionError::Timeout)?
        .map_err(|error| TransactionError::classify_statement(&error))?;
        let token: Option<i64> = timeout(
            self.acquire_timeout,
            sqlx::query_scalar(
                "UPDATE codex_storage.thread_writer_ownership \
                 SET token = token + 1, owner_id = $2::uuid, \
                     lease_until = clock_timestamp() + ($3::bigint * interval '1 millisecond') \
                 WHERE thread_id = $1::uuid AND token < 9223372036854775807 \
                   AND (owner_id IS NULL OR lease_until <= clock_timestamp()) \
                 RETURNING token",
            )
            .bind(thread_id)
            .bind(owner_id)
            .bind(millis)
            .fetch_optional(transaction.connection()),
        )
        .await
        .map_err(|_| TransactionError::Timeout)?
        .map_err(|error| TransactionError::classify_statement(&error))?;
        transaction.commit().await?;
        Ok(token.map(|token| ThreadOwnership {
            thread_id: thread_id.to_owned(),
            owner_id: owner_id.to_owned(),
            token,
        }))
    }

    /// Extend only the current, unexpired token using the server clock.
    pub async fn renew_thread_ownership(
        &self,
        claim: &ThreadOwnership,
        lease: Duration,
    ) -> Result<bool, ThreadOwnershipError> {
        let millis = lease_millis(lease)?;
        let mut transaction = self.begin_serializable().await?;
        let updated = timeout(
            self.acquire_timeout,
            sqlx::query(
                "UPDATE codex_storage.thread_writer_ownership \
                 SET lease_until = clock_timestamp() + ($4::bigint * interval '1 millisecond') \
                 WHERE thread_id = $1::uuid AND owner_id = $2::uuid AND token = $3 \
                   AND lease_until > clock_timestamp()",
            )
            .bind(&claim.thread_id)
            .bind(&claim.owner_id)
            .bind(claim.token)
            .bind(millis)
            .execute(transaction.connection()),
        )
        .await
        .map_err(|_| TransactionError::Timeout)?
        .map_err(|error| TransactionError::classify_statement(&error))?;
        transaction.commit().await?;
        Ok(updated.rows_affected() == 1)
    }

    /// Release only this unexpired token. The row and its counter remain durable.
    pub async fn release_thread_ownership(
        &self,
        claim: &ThreadOwnership,
    ) -> Result<bool, ThreadOwnershipError> {
        let mut transaction = self.begin_serializable().await?;
        let updated = timeout(
            self.acquire_timeout,
            sqlx::query(
                "UPDATE codex_storage.thread_writer_ownership \
                 SET owner_id = NULL, lease_until = NULL \
                 WHERE thread_id = $1::uuid AND owner_id = $2::uuid AND token = $3 \
                   AND lease_until > clock_timestamp()",
            )
            .bind(&claim.thread_id)
            .bind(&claim.owner_id)
            .bind(claim.token)
            .execute(transaction.connection()),
        )
        .await
        .map_err(|_| TransactionError::Timeout)?
        .map_err(|error| TransactionError::classify_statement(&error))?;
        transaction.commit().await?;
        Ok(updated.rows_affected() == 1)
    }

    /// Readback for an ambiguous claim result. This is not write authorization.
    pub async fn observe_thread_ownership(
        &self,
        claim: &ThreadOwnership,
    ) -> Result<bool, ThreadOwnershipError> {
        let mut connection = self.acquire().await.map_err(|error| {
            ThreadOwnershipError::Transaction(match error {
                crate::PoolError::Timeout => TransactionError::Timeout,
                _ => TransactionError::Unavailable,
            })
        })?;
        timeout(
            self.acquire_timeout,
            sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM codex_storage.thread_writer_ownership \
                 WHERE thread_id = $1::uuid AND owner_id = $2::uuid AND token = $3 \
                   AND lease_until > clock_timestamp())",
            )
            .bind(&claim.thread_id)
            .bind(&claim.owner_id)
            .bind(claim.token)
            .fetch_one(&mut *connection),
        )
        .await
        .map_err(|_| TransactionError::Timeout)?
        .map_err(|error| TransactionError::classify_statement(&error))
        .map_err(Into::into)
    }

    /// Resolve an uncertain claim COMMIT by its unique owner attempt ID.
    /// This readback is not write authorization or proof of a prior mutation.
    pub async fn recover_thread_ownership(
        &self,
        thread_id: &str,
        owner_id: &str,
    ) -> Result<Option<ThreadOwnership>, ThreadOwnershipError> {
        let mut connection = self.acquire().await.map_err(|error| {
            ThreadOwnershipError::Transaction(match error {
                crate::PoolError::Timeout => TransactionError::Timeout,
                _ => TransactionError::Unavailable,
            })
        })?;
        let token = timeout(
            self.acquire_timeout,
            sqlx::query_scalar(
                "SELECT token FROM codex_storage.thread_writer_ownership \
                 WHERE thread_id = $1::uuid AND owner_id = $2::uuid \
                   AND lease_until > clock_timestamp()",
            )
            .bind(thread_id)
            .bind(owner_id)
            .fetch_optional(&mut *connection),
        )
        .await
        .map_err(|_| TransactionError::Timeout)?
        .map_err(|error| TransactionError::classify_statement(&error))?;
        Ok(token.map(|token| ThreadOwnership {
            thread_id: thread_id.to_owned(),
            owner_id: owner_id.to_owned(),
            token,
        }))
    }
}
