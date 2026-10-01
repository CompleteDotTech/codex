//! Storage-neutral goal persistence used by the state runtime's callers.

use std::future::Future;
use std::pin::Pin;

use codex_protocol::ThreadId;

use super::goals::GoalAccountingMode;
use super::goals::GoalAccountingOutcome;
use super::goals::GoalStore;
use super::goals::GoalUpdate;
use crate::ThreadGoal;
use crate::ThreadGoalStatus;

/// Future returned by goal persistence operations.
pub type GoalStoreFuture<'a, T> = Pin<Box<dyn Future<Output = anyhow::Result<T>> + Send + 'a>>;

/// Durable goal operations shared by local and future remote state runtimes.
///
/// Implementations must preserve goal IDs and timestamps when replacing a snapshot,
/// and insert its continuation deferral in the same transaction. Expected-goal-ID
/// checks must be atomic with mutations and return the local store's outcome for a
/// missing or superseded goal. A no-op update only reads and checks the current goal;
/// zero-delta usage accounting reads it without checking the expected ID.
/// An acknowledged mutation must be durable and visible to a subsequent read.
pub trait ThreadGoalStore: Send + Sync {
    fn get_thread_goal(&self, thread_id: ThreadId) -> GoalStoreFuture<'_, Option<ThreadGoal>>;

    fn replace_thread_goal_snapshot<'a>(&'a self, goal: &'a ThreadGoal) -> GoalStoreFuture<'a, ()>;

    fn has_thread_goal_continuation_deferral(
        &self,
        thread_id: ThreadId,
    ) -> GoalStoreFuture<'_, bool>;

    fn clear_thread_goal_continuation_deferral(
        &self,
        thread_id: ThreadId,
    ) -> GoalStoreFuture<'_, ()>;

    fn replace_thread_goal<'a>(
        &'a self,
        thread_id: ThreadId,
        objective: &'a str,
        status: ThreadGoalStatus,
        token_budget: Option<i64>,
    ) -> GoalStoreFuture<'a, ThreadGoal>;

    fn insert_thread_goal<'a>(
        &'a self,
        thread_id: ThreadId,
        objective: &'a str,
        status: ThreadGoalStatus,
        token_budget: Option<i64>,
    ) -> GoalStoreFuture<'a, Option<ThreadGoal>>;

    fn update_thread_goal(
        &self,
        thread_id: ThreadId,
        update: GoalUpdate,
    ) -> GoalStoreFuture<'_, Option<ThreadGoal>>;

    fn pause_active_thread_goal(
        &self,
        thread_id: ThreadId,
    ) -> GoalStoreFuture<'_, Option<ThreadGoal>>;

    fn usage_limit_active_thread_goal(
        &self,
        thread_id: ThreadId,
    ) -> GoalStoreFuture<'_, Option<ThreadGoal>>;

    fn delete_thread_goal(&self, thread_id: ThreadId) -> GoalStoreFuture<'_, Option<ThreadGoal>>;

    fn account_thread_goal_usage<'a>(
        &'a self,
        thread_id: ThreadId,
        time_delta_seconds: i64,
        token_delta: i64,
        mode: GoalAccountingMode,
        expected_goal_id: Option<&'a str>,
    ) -> GoalStoreFuture<'a, GoalAccountingOutcome>;
}

impl ThreadGoalStore for GoalStore {
    fn get_thread_goal(&self, thread_id: ThreadId) -> GoalStoreFuture<'_, Option<ThreadGoal>> {
        Box::pin(GoalStore::get_thread_goal(self, thread_id))
    }

    fn replace_thread_goal_snapshot<'a>(&'a self, goal: &'a ThreadGoal) -> GoalStoreFuture<'a, ()> {
        Box::pin(GoalStore::replace_thread_goal_snapshot(self, goal))
    }

    fn has_thread_goal_continuation_deferral(
        &self,
        thread_id: ThreadId,
    ) -> GoalStoreFuture<'_, bool> {
        Box::pin(GoalStore::has_thread_goal_continuation_deferral(
            self, thread_id,
        ))
    }

    fn clear_thread_goal_continuation_deferral(
        &self,
        thread_id: ThreadId,
    ) -> GoalStoreFuture<'_, ()> {
        Box::pin(GoalStore::clear_thread_goal_continuation_deferral(
            self, thread_id,
        ))
    }

    fn replace_thread_goal<'a>(
        &'a self,
        thread_id: ThreadId,
        objective: &'a str,
        status: ThreadGoalStatus,
        token_budget: Option<i64>,
    ) -> GoalStoreFuture<'a, ThreadGoal> {
        Box::pin(GoalStore::replace_thread_goal(
            self,
            thread_id,
            objective,
            status,
            token_budget,
        ))
    }

    fn insert_thread_goal<'a>(
        &'a self,
        thread_id: ThreadId,
        objective: &'a str,
        status: ThreadGoalStatus,
        token_budget: Option<i64>,
    ) -> GoalStoreFuture<'a, Option<ThreadGoal>> {
        Box::pin(GoalStore::insert_thread_goal(
            self,
            thread_id,
            objective,
            status,
            token_budget,
        ))
    }

    fn update_thread_goal(
        &self,
        thread_id: ThreadId,
        update: GoalUpdate,
    ) -> GoalStoreFuture<'_, Option<ThreadGoal>> {
        Box::pin(GoalStore::update_thread_goal(self, thread_id, update))
    }

    fn pause_active_thread_goal(
        &self,
        thread_id: ThreadId,
    ) -> GoalStoreFuture<'_, Option<ThreadGoal>> {
        Box::pin(GoalStore::pause_active_thread_goal(self, thread_id))
    }

    fn usage_limit_active_thread_goal(
        &self,
        thread_id: ThreadId,
    ) -> GoalStoreFuture<'_, Option<ThreadGoal>> {
        Box::pin(GoalStore::usage_limit_active_thread_goal(self, thread_id))
    }

    fn delete_thread_goal(&self, thread_id: ThreadId) -> GoalStoreFuture<'_, Option<ThreadGoal>> {
        Box::pin(GoalStore::delete_thread_goal(self, thread_id))
    }

    fn account_thread_goal_usage<'a>(
        &'a self,
        thread_id: ThreadId,
        time_delta_seconds: i64,
        token_delta: i64,
        mode: GoalAccountingMode,
        expected_goal_id: Option<&'a str>,
    ) -> GoalStoreFuture<'a, GoalAccountingOutcome> {
        Box::pin(GoalStore::account_thread_goal_usage(
            self,
            thread_id,
            time_delta_seconds,
            token_delta,
            mode,
            expected_goal_id,
        ))
    }
}
