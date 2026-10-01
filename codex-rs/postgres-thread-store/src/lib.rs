//! Unwired fixed-namespace PostgreSQL implementation of the thread store.

mod adapters;
mod live;
mod meta;
mod store;

pub use store::PostgresThreadStore;
