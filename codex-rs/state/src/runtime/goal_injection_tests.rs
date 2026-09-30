use super::*;
use crate::runtime::test_support::unique_temp_dir;
use codex_utils_absolute_path::test_support::PathExt;
use pretty_assertions::assert_eq;

#[tokio::test]
async fn injected_goal_store_routes_reads_and_mutations_without_local_goal_writes() {
    let backend_home = unique_temp_dir();
    let local_home = unique_temp_dir();
    let backend = StateRuntime::init(
        SqliteConfig::new_for_testing(backend_home.as_path().abs()),
        "test-provider".to_string(),
    )
    .await
    .unwrap();
    let local = StateRuntime::init_with_goal_store(
        SqliteConfig::new_for_testing(local_home.as_path().abs()),
        "test-provider".to_string(),
        Arc::clone(&backend.thread_goals),
    )
    .await
    .unwrap();
    let thread_id = ThreadId::from_string("00000000-0000-0000-0000-000000000123").unwrap();

    let created = local
        .thread_goals()
        .replace_thread_goal(
            thread_id,
            "finish the migration",
            crate::ThreadGoalStatus::Active,
            /*token_budget*/ Some(100),
        )
        .await
        .unwrap();
    assert_eq!(
        Some(created.clone()),
        backend
            .thread_goals()
            .get_thread_goal(thread_id)
            .await
            .unwrap()
    );

    local
        .thread_goals()
        .account_thread_goal_usage(
            thread_id,
            /*time_delta_seconds*/ 2,
            /*token_delta*/ 7,
            GoalAccountingMode::ActiveOnly,
            /*expected_goal_id*/ Some(&created.goal_id),
        )
        .await
        .unwrap();
    assert_eq!(
        local
            .thread_goals()
            .get_thread_goal(thread_id)
            .await
            .unwrap(),
        backend
            .thread_goals()
            .get_thread_goal(thread_id)
            .await
            .unwrap()
    );

    assert_eq!(
        local
            .local_thread_goals
            .get_thread_goal(thread_id)
            .await
            .unwrap(),
        None
    );

    let backend_goal = backend
        .thread_goals()
        .get_thread_goal(thread_id)
        .await
        .unwrap();
    local.close().await;
    assert_eq!(
        backend
            .thread_goals()
            .get_thread_goal(thread_id)
            .await
            .unwrap(),
        backend_goal
    );
    backend.close().await;
    let _ = tokio::fs::remove_dir_all(local_home).await;
    let _ = tokio::fs::remove_dir_all(backend_home).await;
}
