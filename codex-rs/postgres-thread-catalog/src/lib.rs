//! Unwired fixed-namespace PostgreSQL persistence for the thread catalog.

mod catalog;
mod list;
mod projects;
mod timestamps;

pub use catalog::PostgresThreadCatalog;
