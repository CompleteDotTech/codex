//! Unwired fixed-namespace PostgreSQL persistence for generated memory and its job leases.

mod postgres;

pub use postgres::PostgresMemoryStore;
pub use postgres::delete_thread_memory_in;
pub use postgres::lock_memory_in;
