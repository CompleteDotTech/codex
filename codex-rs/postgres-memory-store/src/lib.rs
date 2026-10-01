//! Unwired fixed-namespace PostgreSQL persistence for generated memory and its job leases.

#[macro_use]
mod threads;
mod postgres;

pub use postgres::PostgresMemoryStore;
