//! Unwired fixed-namespace PostgreSQL persistence for queued user messages.

mod postgres;

pub use postgres::PostgresQueueStore;
pub use postgres::delete_thread_queue_in;
