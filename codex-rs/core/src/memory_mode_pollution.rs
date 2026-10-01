//! Routes external-context memory pollution through the selected local version.

use codex_protocol::MemoryVersion;
use codex_protocol::ThreadId;
use codex_state::StateRuntime;
use futures::future::BoxFuture;

/// Preserve the current turn behavior on persistence failure while making the
/// failed mutation visible. StateRuntime rejects injected stores before mutation;
/// this logging boundary is not a gate for activating them.
pub(crate) fn mark_thread_memory_mode_polluted<'a>(
    state_db: Option<&'a StateRuntime>,
    version: MemoryVersion,
    thread_id: ThreadId,
    stage: &'static str,
) -> BoxFuture<'a, ()> {
    Box::pin(async move {
        let Some(state_db) = state_db else {
            return;
        };
        if let Err(error) = state_db
            .mark_thread_memory_mode_polluted_for_version(version, thread_id)
            .await
        {
            tracing::warn!(%stage, ?version, %thread_id, %error, "failed to mark thread memory mode polluted");
        }
    })
}
