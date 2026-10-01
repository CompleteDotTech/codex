//! Storage-neutral persistence boundary for runtime logs and feedback export.

use std::future::Future;
use std::pin::Pin;

use super::StateRuntime;
use crate::LogEntry;
use crate::LogQuery;
use crate::LogRow;

/// Future returned by a runtime log operation.
pub type LogStoreFuture<'a, T> = Pin<Box<dyn Future<Output = anyhow::Result<T>> + Send + 'a>>;

/// Durable log operations shared by local and future remote runtimes.
///
/// Implementations must commit each batch atomically with its retention pruning.
/// Queries must apply the same `LogQuery` filters and stable ID order as the local
/// store. Feedback export must include the latest associated process logs, preserve
/// chronological output, and enforce the existing whole-line byte cap. An
/// acknowledged write must be visible to a subsequent read.
pub trait RuntimeLogStore: Send + Sync {
    fn insert_logs<'a>(&'a self, entries: &'a [LogEntry]) -> LogStoreFuture<'a, ()>;

    fn query_logs<'a>(&'a self, query: &'a LogQuery) -> LogStoreFuture<'a, Vec<LogRow>>;

    fn query_feedback_logs_for_threads<'a>(
        &'a self,
        thread_ids: &'a [&str],
    ) -> LogStoreFuture<'a, Vec<u8>>;

    fn max_log_id<'a>(&'a self, query: &'a LogQuery) -> LogStoreFuture<'a, i64>;
}

impl RuntimeLogStore for StateRuntime {
    fn insert_logs<'a>(&'a self, entries: &'a [LogEntry]) -> LogStoreFuture<'a, ()> {
        Box::pin(StateRuntime::insert_logs(self, entries))
    }

    fn query_logs<'a>(&'a self, query: &'a LogQuery) -> LogStoreFuture<'a, Vec<LogRow>> {
        Box::pin(StateRuntime::query_logs(self, query))
    }

    fn query_feedback_logs_for_threads<'a>(
        &'a self,
        thread_ids: &'a [&str],
    ) -> LogStoreFuture<'a, Vec<u8>> {
        Box::pin(StateRuntime::query_feedback_logs_for_threads(
            self, thread_ids,
        ))
    }

    fn max_log_id<'a>(&'a self, query: &'a LogQuery) -> LogStoreFuture<'a, i64> {
        Box::pin(StateRuntime::max_log_id(self, query))
    }
}
