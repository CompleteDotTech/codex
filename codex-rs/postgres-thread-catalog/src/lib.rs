//! Unwired fixed-namespace PostgreSQL persistence for the thread catalog.

mod attachments;
mod catalog;
mod list;
mod projects;
mod sections;
mod timestamps;

pub use catalog::PostgresThreadCatalog;
pub use catalog::get_thread_in;
pub use catalog::upsert_thread_in;
