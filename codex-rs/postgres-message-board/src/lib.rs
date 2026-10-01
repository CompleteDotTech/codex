//! Unwired fixed-namespace PostgreSQL persistence for the agent message board.

mod board;
mod lifecycle;
mod paging;
mod queries;

pub use board::PostgresAgentMessageBoard;
