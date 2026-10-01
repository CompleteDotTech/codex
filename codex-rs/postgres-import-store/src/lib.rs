//! Unwired fixed-namespace PostgreSQL persistence for external-agent import records.

mod postgres;

pub use postgres::ImportStoreError;
pub use postgres::PostgresExternalAgentImportStore;
