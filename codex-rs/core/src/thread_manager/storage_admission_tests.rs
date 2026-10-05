//! Actual production per-tree membership/seal controls, not full writer drain.
use super::*;
use crate::config::test_config;
use std::time::Duration;
async fn manager() -> Arc<ThreadManager> {
    let config = test_config().await;
    Arc::new(ThreadManager::with_models_provider_and_home_for_tests(
        CodexAuth::from_api_key("dummy"),
        config.model_provider.clone(),
        config.codex_home.to_path_buf(),
        Arc::new(EnvironmentManager::default_for_tests()),
    ))
}

async fn manager_with_live_home() -> (tempfile::TempDir, Arc<ThreadManager>, Config) {
    let home = tempfile::tempdir().expect("create manager Codex home");
    let mut config = test_config().await;
    config.codex_home =
        AbsolutePathBuf::from_absolute_path(home.path()).expect("manager home should be absolute");
    config.cwd = config.codex_home.clone();
    let manager = Arc::new(ThreadManager::with_models_provider_and_home_for_tests(
        CodexAuth::from_api_key("dummy"),
        config.model_provider.clone(),
        home.path().to_path_buf(),
        Arc::new(EnvironmentManager::default_for_tests()),
    ));
    (home, manager, config)
}
#[tokio::test]
async fn actual_root_and_delegate_memberships_block_join_and_seal_future_roots()
-> anyhow::Result<()> {
    let manager = manager().await;
    let root = manager.agent_control().runtime;
    let root_member = root.admit_start()?.into_teardown_guard();
    let delegate = root.clone(); // the exact same original production tree
    let delegate_member = delegate.admit_start()?.into_teardown_guard();
    let other = manager.agent_control().runtime;
    let other_member = other.admit_start()?.into_teardown_guard();
    anyhow::ensure!(
        manager
            .state
            .storage_sessions
            .admission
            .lock()
            .map_err(|_| io::Error::other("poisoned"))?
            .trees
            .len()
            == 2
    );
    // Real TaskTracker memberships are retained; neither timeout nor closed gate
    // supplies joined evidence. No synthetic Probe/Future substitutes teardown.
    anyhow::ensure!(
        tokio::time::timeout(
            Duration::from_millis(10),
            manager.seal_and_join_session_trees()
        )
        .await
        .is_err()
    );
    anyhow::ensure!(root.admit_start().is_err());
    anyhow::ensure!(delegate.admit_start().is_err());
    anyhow::ensure!(manager.agent_control().runtime.admit_start().is_err());
    root_member.complete();
    anyhow::ensure!(
        tokio::time::timeout(
            Duration::from_millis(10),
            manager.seal_and_join_session_trees()
        )
        .await
        .is_err()
    );
    delegate_member.complete();
    anyhow::ensure!(
        tokio::time::timeout(
            Duration::from_millis(10),
            manager.seal_and_join_session_trees()
        )
        .await
        .is_err()
    );
    other_member.complete();
    let evidence = tokio::time::timeout(
        Duration::from_secs(1),
        manager.seal_and_join_session_trees(),
    )
    .await??;
    anyhow::ensure!(Arc::ptr_eq(&evidence._original, &manager));
    anyhow::ensure!(
        manager
            .state
            .storage_sessions
            .admission
            .lock()
            .map_err(|_| io::Error::other("poisoned"))?
            .sealed
    );
    Ok(())
}
#[tokio::test]
async fn actual_failed_teardown_never_joins_and_error_retains_last_manager() -> anyhow::Result<()> {
    let manager = manager().await;
    let weak = Arc::downgrade(&manager);
    let root = manager.agent_control().runtime;
    let failed = root.admit_start()?.into_teardown_guard();
    drop(failed); // actual producer records incomplete teardown before token drop
    let error = match tokio::time::timeout(
        Duration::from_secs(1),
        manager.seal_and_join_session_trees(),
    )
    .await?
    {
        Err(error) => error,
        Ok(_) => anyhow::bail!("actual failed membership produced joined evidence"),
    };
    let held = error
        .get_ref()
        .and_then(|cause| cause.downcast_ref::<JoinError>())
        .ok_or_else(|| io::Error::other("original manager error owner absent"))?;
    anyhow::ensure!(Arc::ptr_eq(&held._original, &manager));
    drop(manager);
    anyhow::ensure!(weak.upgrade().is_some());
    drop(error);
    anyhow::ensure!(weak.upgrade().is_none());
    Ok(())
}
#[tokio::test]
async fn explicit_home_mismatch_refuses_before_actual_tree_membership() -> anyhow::Result<()> {
    let manager = manager().await;
    let other = manager.state.storage_sessions.home.join("different-home");
    anyhow::ensure!(manager.state.storage_sessions.require_home(&other).is_err());
    anyhow::ensure!(
        manager
            .state
            .storage_sessions
            .admission
            .lock()
            .map_err(|_| io::Error::other("poisoned"))?
            .trees
            .is_empty()
    );
    Ok(())
}

#[tokio::test]
async fn public_root_start_rejects_a_different_manager_home_before_retention() -> anyhow::Result<()>
{
    let (_home, manager, mut config) = manager_with_live_home().await;
    let wrong_home = tempfile::tempdir().expect("create distinct session home");
    config.codex_home = AbsolutePathBuf::from_absolute_path(wrong_home.path())
        .expect("session home should be absolute");
    config.cwd = config.codex_home.clone();

    let error = match manager.start_thread(StartThreadOptions::new(config)).await {
        Ok(_) => anyhow::bail!("public root startup accepted a different manager home"),
        Err(error) => error,
    };
    anyhow::ensure!(
        matches!(&error, CodexErr::InvalidRequest(message) if message.as_str() == "session home differs from its manager"),
        "public root startup returned an unexpected error: {error}"
    );
    anyhow::ensure!(manager.list_thread_ids().await.is_empty());
    anyhow::ensure!(
        manager
            .state
            .storage_sessions
            .admission
            .lock()
            .map_err(|_| io::Error::other("poisoned"))?
            .trees
            .is_empty(),
        "rejected root startup must not retain a tree owner"
    );
    Ok(())
}

#[tokio::test]
async fn public_delegate_start_rejects_a_different_manager_home_without_retaining()
-> anyhow::Result<()> {
    let (_home, manager, config) = manager_with_live_home().await;
    let parent = manager
        .start_thread(StartThreadOptions::new(config.clone()))
        .await?;
    let wrong_home = tempfile::tempdir().expect("create distinct delegate home");
    let mut wrong_config = config;
    wrong_config.codex_home = AbsolutePathBuf::from_absolute_path(wrong_home.path())
        .expect("delegate home should be absolute");
    wrong_config.cwd = wrong_config.codex_home.clone();
    let mut options = StartThreadOptions::new(wrong_config);
    options.session_source = Some(SessionSource::Internal(
        InternalSessionSource::MemoryConsolidation,
    ));

    let error = match manager
        .spawn_internal_session(parent.thread_id, options)
        .await
    {
        Ok(_) => anyhow::bail!("public delegate startup accepted a different manager home"),
        Err(error) => error,
    };
    anyhow::ensure!(
        matches!(&error, CodexErr::InvalidRequest(message) if message.as_str() == "session home differs from its manager"),
        "public delegate startup returned an unexpected error: {error}"
    );
    anyhow::ensure!(manager.list_thread_ids().await == vec![parent.thread_id]);
    anyhow::ensure!(
        manager
            .state
            .storage_sessions
            .admission
            .lock()
            .map_err(|_| io::Error::other("poisoned"))?
            .trees
            .len()
            == 1,
        "rejected delegate startup must not retain another tree owner"
    );
    let shutdown = manager
        .shutdown_all_threads_bounded(Duration::from_secs(10))
        .await;
    anyhow::ensure!(shutdown.timed_out.is_empty());
    anyhow::ensure!(shutdown.submit_failed.is_empty());
    Ok(())
}

#[tokio::test]
async fn actual_retain_to_membership_race_cannot_admit_after_manager_seal() -> anyhow::Result<()> {
    let manager = manager().await;
    let root = manager.agent_control().runtime;
    let (entered_send, entered_receive) = std::sync::mpsc::channel();
    let (release_send, release_receive) = std::sync::mpsc::channel();
    *manager
        .state
        .storage_sessions
        .after_retain
        .lock()
        .map_err(|_| io::Error::other("seam poisoned"))? = Some(Arc::new(RetainPause {
        owner: root.clone(),
        entered: entered_send,
        release: Mutex::new(release_receive),
        fired: std::sync::atomic::AtomicBool::new(false),
    }));
    let racing = std::thread::spawn(move || root.admit_start());
    // Always release and join the real admission thread BEFORE asserting anything.
    // Even a failed witness/timeout must not strand this test's native thread.
    let witnessed = entered_receive.recv_timeout(Duration::from_secs(1));
    let sealed = manager.state.storage_sessions.seal();
    let released = release_send.send(());
    let joined = racing
        .join()
        .map_err(|_| io::Error::other("admission thread panicked"))?;
    witnessed?;
    let states = sealed?;
    released?;
    anyhow::ensure!(joined.is_err(), "actual membership escaped after seal");
    anyhow::ensure!(states.len() == 1);
    tokio::time::timeout(Duration::from_secs(1), states[0].wait()).await??;
    anyhow::ensure!(manager.agent_control().runtime.admit_start().is_err());
    Ok(())
}
