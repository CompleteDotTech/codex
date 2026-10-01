//! Unwired fixed-namespace PostgreSQL persistence for thread goals.

mod postgres;

pub use postgres::GoalStoreError;
pub use postgres::PostgresGoalStore;
