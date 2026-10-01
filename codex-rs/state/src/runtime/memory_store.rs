//! Storage-neutral operations for generated memory and its job leases.

use std::future::Future;
use std::pin::Pin;

use codex_protocol::ThreadId;

use super::MemoryStore;
use crate::Phase2JobClaimOutcome;
use crate::Stage1JobClaim;
use crate::Stage1JobClaimOutcome;
use crate::Stage1Output;
use crate::Stage1StartupClaimParams;

/// Future returned by a memory persistence operation.
pub type MemoryStoreFuture<'a, T> = Pin<Box<dyn Future<Output = anyhow::Result<T>> + Send + 'a>>;

/// Durable operations for one version of the generated-memory store.
///
/// Implementations must preserve ownership-token and lease checks when claiming,
/// heartbeating, or completing jobs. Completing a stage-1 output, removing an
/// output selected for phase 2, and enqueueing the resulting phase-2 work must
/// each keep the local store's same-transaction behavior. Phase-2 completion
/// must atomically update its job and the exact selected output snapshots.
/// Reads of the separate thread catalog and changes to thread memory mode need
/// explicit cross-store coordination before any remote backend can be activated.
/// An acknowledged mutation must be durable and visible to a later read.
pub trait RuntimeMemoryStore: Send + Sync {
    fn clear_memory_data(&self) -> MemoryStoreFuture<'_, ()>;

    fn delete_thread_memory(&self, thread_id: ThreadId) -> MemoryStoreFuture<'_, ()>;

    fn record_stage1_output_usage<'a>(
        &'a self,
        thread_ids: &'a [ThreadId],
    ) -> MemoryStoreFuture<'a, usize>;

    fn claim_stage1_jobs_for_startup<'a>(
        &'a self,
        current_thread_id: ThreadId,
        params: Stage1StartupClaimParams<'a>,
    ) -> MemoryStoreFuture<'a, Vec<Stage1JobClaim>>;

    fn list_stage1_outputs_for_global(&self, n: usize) -> MemoryStoreFuture<'_, Vec<Stage1Output>>;

    fn prune_stage1_outputs_for_retention(
        &self,
        max_unused_days: i64,
        limit: usize,
    ) -> MemoryStoreFuture<'_, usize>;

    fn get_phase2_input_selection(
        &self,
        n: usize,
        max_unused_days: i64,
    ) -> MemoryStoreFuture<'_, Vec<Stage1Output>>;

    fn mark_thread_memory_mode_polluted(&self, thread_id: ThreadId) -> MemoryStoreFuture<'_, bool>;

    fn try_claim_stage1_job(
        &self,
        thread_id: ThreadId,
        worker_id: ThreadId,
        source_updated_at: i64,
        lease_seconds: i64,
        max_running_jobs: usize,
    ) -> MemoryStoreFuture<'_, Stage1JobClaimOutcome>;

    fn mark_stage1_job_succeeded<'a>(
        &'a self,
        thread_id: ThreadId,
        ownership_token: &'a str,
        source_updated_at: i64,
        raw_memory: &'a str,
        rollout_summary: &'a str,
        rollout_slug: Option<&'a str>,
    ) -> MemoryStoreFuture<'a, bool>;

    fn mark_stage1_job_succeeded_no_output<'a>(
        &'a self,
        thread_id: ThreadId,
        ownership_token: &'a str,
    ) -> MemoryStoreFuture<'a, bool>;

    fn mark_stage1_job_failed<'a>(
        &'a self,
        thread_id: ThreadId,
        ownership_token: &'a str,
        failure_reason: &'a str,
        retry_delay_seconds: i64,
    ) -> MemoryStoreFuture<'a, bool>;

    fn enqueue_global_consolidation(&self, input_watermark: i64) -> MemoryStoreFuture<'_, ()>;

    fn try_claim_global_phase2_job(
        &self,
        worker_id: ThreadId,
        lease_seconds: i64,
    ) -> MemoryStoreFuture<'_, Phase2JobClaimOutcome>;

    fn heartbeat_global_phase2_job<'a>(
        &'a self,
        ownership_token: &'a str,
        lease_seconds: i64,
    ) -> MemoryStoreFuture<'a, bool>;

    fn mark_global_phase2_job_succeeded<'a>(
        &'a self,
        ownership_token: &'a str,
        completed_watermark: i64,
        selected_outputs: &'a [Stage1Output],
    ) -> MemoryStoreFuture<'a, bool>;

    fn mark_global_phase2_job_failed<'a>(
        &'a self,
        ownership_token: &'a str,
        failure_reason: &'a str,
        retry_delay_seconds: i64,
    ) -> MemoryStoreFuture<'a, bool>;

    fn mark_global_phase2_job_failed_if_unowned<'a>(
        &'a self,
        ownership_token: &'a str,
        failure_reason: &'a str,
        retry_delay_seconds: i64,
    ) -> MemoryStoreFuture<'a, bool>;

    fn max_consolidated_thread_count(&self) -> MemoryStoreFuture<'_, u32>;
}

impl RuntimeMemoryStore for MemoryStore {
    fn clear_memory_data(&self) -> MemoryStoreFuture<'_, ()> {
        Box::pin(MemoryStore::clear_memory_data(self))
    }

    fn delete_thread_memory(&self, thread_id: ThreadId) -> MemoryStoreFuture<'_, ()> {
        Box::pin(MemoryStore::delete_thread_memory(self, thread_id))
    }

    fn record_stage1_output_usage<'a>(
        &'a self,
        thread_ids: &'a [ThreadId],
    ) -> MemoryStoreFuture<'a, usize> {
        Box::pin(MemoryStore::record_stage1_output_usage(self, thread_ids))
    }

    fn claim_stage1_jobs_for_startup<'a>(
        &'a self,
        current_thread_id: ThreadId,
        params: Stage1StartupClaimParams<'a>,
    ) -> MemoryStoreFuture<'a, Vec<Stage1JobClaim>> {
        Box::pin(MemoryStore::claim_stage1_jobs_for_startup(
            self,
            current_thread_id,
            params,
        ))
    }

    fn list_stage1_outputs_for_global(&self, n: usize) -> MemoryStoreFuture<'_, Vec<Stage1Output>> {
        Box::pin(MemoryStore::list_stage1_outputs_for_global(self, n))
    }

    fn prune_stage1_outputs_for_retention(
        &self,
        max_unused_days: i64,
        limit: usize,
    ) -> MemoryStoreFuture<'_, usize> {
        Box::pin(MemoryStore::prune_stage1_outputs_for_retention(
            self,
            max_unused_days,
            limit,
        ))
    }

    fn get_phase2_input_selection(
        &self,
        n: usize,
        max_unused_days: i64,
    ) -> MemoryStoreFuture<'_, Vec<Stage1Output>> {
        Box::pin(MemoryStore::get_phase2_input_selection(
            self,
            n,
            max_unused_days,
        ))
    }

    fn mark_thread_memory_mode_polluted(&self, thread_id: ThreadId) -> MemoryStoreFuture<'_, bool> {
        Box::pin(MemoryStore::mark_thread_memory_mode_polluted(
            self, thread_id,
        ))
    }

    fn try_claim_stage1_job(
        &self,
        thread_id: ThreadId,
        worker_id: ThreadId,
        source_updated_at: i64,
        lease_seconds: i64,
        max_running_jobs: usize,
    ) -> MemoryStoreFuture<'_, Stage1JobClaimOutcome> {
        Box::pin(MemoryStore::try_claim_stage1_job(
            self,
            thread_id,
            worker_id,
            source_updated_at,
            lease_seconds,
            max_running_jobs,
        ))
    }

    fn mark_stage1_job_succeeded<'a>(
        &'a self,
        thread_id: ThreadId,
        ownership_token: &'a str,
        source_updated_at: i64,
        raw_memory: &'a str,
        rollout_summary: &'a str,
        rollout_slug: Option<&'a str>,
    ) -> MemoryStoreFuture<'a, bool> {
        Box::pin(MemoryStore::mark_stage1_job_succeeded(
            self,
            thread_id,
            ownership_token,
            source_updated_at,
            raw_memory,
            rollout_summary,
            rollout_slug,
        ))
    }

    fn mark_stage1_job_succeeded_no_output<'a>(
        &'a self,
        thread_id: ThreadId,
        ownership_token: &'a str,
    ) -> MemoryStoreFuture<'a, bool> {
        Box::pin(MemoryStore::mark_stage1_job_succeeded_no_output(
            self,
            thread_id,
            ownership_token,
        ))
    }

    fn mark_stage1_job_failed<'a>(
        &'a self,
        thread_id: ThreadId,
        ownership_token: &'a str,
        failure_reason: &'a str,
        retry_delay_seconds: i64,
    ) -> MemoryStoreFuture<'a, bool> {
        Box::pin(MemoryStore::mark_stage1_job_failed(
            self,
            thread_id,
            ownership_token,
            failure_reason,
            retry_delay_seconds,
        ))
    }

    fn enqueue_global_consolidation(&self, input_watermark: i64) -> MemoryStoreFuture<'_, ()> {
        Box::pin(MemoryStore::enqueue_global_consolidation(
            self,
            input_watermark,
        ))
    }

    fn try_claim_global_phase2_job(
        &self,
        worker_id: ThreadId,
        lease_seconds: i64,
    ) -> MemoryStoreFuture<'_, Phase2JobClaimOutcome> {
        Box::pin(MemoryStore::try_claim_global_phase2_job(
            self,
            worker_id,
            lease_seconds,
        ))
    }

    fn heartbeat_global_phase2_job<'a>(
        &'a self,
        ownership_token: &'a str,
        lease_seconds: i64,
    ) -> MemoryStoreFuture<'a, bool> {
        Box::pin(MemoryStore::heartbeat_global_phase2_job(
            self,
            ownership_token,
            lease_seconds,
        ))
    }

    fn mark_global_phase2_job_succeeded<'a>(
        &'a self,
        ownership_token: &'a str,
        completed_watermark: i64,
        selected_outputs: &'a [Stage1Output],
    ) -> MemoryStoreFuture<'a, bool> {
        Box::pin(MemoryStore::mark_global_phase2_job_succeeded(
            self,
            ownership_token,
            completed_watermark,
            selected_outputs,
        ))
    }

    fn mark_global_phase2_job_failed<'a>(
        &'a self,
        ownership_token: &'a str,
        failure_reason: &'a str,
        retry_delay_seconds: i64,
    ) -> MemoryStoreFuture<'a, bool> {
        Box::pin(MemoryStore::mark_global_phase2_job_failed(
            self,
            ownership_token,
            failure_reason,
            retry_delay_seconds,
        ))
    }

    fn mark_global_phase2_job_failed_if_unowned<'a>(
        &'a self,
        ownership_token: &'a str,
        failure_reason: &'a str,
        retry_delay_seconds: i64,
    ) -> MemoryStoreFuture<'a, bool> {
        Box::pin(MemoryStore::mark_global_phase2_job_failed_if_unowned(
            self,
            ownership_token,
            failure_reason,
            retry_delay_seconds,
        ))
    }

    fn max_consolidated_thread_count(&self) -> MemoryStoreFuture<'_, u32> {
        Box::pin(MemoryStore::max_consolidated_thread_count(self))
    }
}
