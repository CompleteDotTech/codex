//! Unwired fixed-namespace PostgreSQL persistence for runtime logs and feedback export.

mod postgres;

pub use postgres::PostgresLogStore;
