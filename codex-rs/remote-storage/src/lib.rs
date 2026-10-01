//! One connection to a remote PostgreSQL dataset and the stores that live on it.
//!
//! A [`RemoteStorage`] is built from a trusted storage profile and a host credential resolver.
//! It connects with the runtime login, verifies that the store holds the schema format this build
//! writes, and then hands out the PostgreSQL implementations of every store trait. It never
//! creates tables, never falls back to local files, and reports failures as typed outcomes that
//! say whether trying again can help.

mod error;
mod storage;

pub use error::RemoteStorageError;
pub use storage::RemoteStorage;
pub use storage::StorageActivation;
