//! Unwired fixed-namespace PostgreSQL persistence for the thread catalog.

mod catalog;
mod timestamps;

pub use catalog::PostgresThreadCatalog;
