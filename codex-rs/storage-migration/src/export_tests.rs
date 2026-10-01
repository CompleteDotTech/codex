//! PostgreSQL to SQLite: the activated dataset is written into a staged home, verified by digest,
//! and opens with the ordinary runtime.

use super::tests::reset_target;
use super::*;
use codex_state::SqliteConfig;
use codex_state::StateRuntime;
use codex_state::ThreadMetadata;
use codex_utils_absolute_path::test_support::PathExt;
use pretty_assertions::assert_eq;
use uuid::Uuid;

/// A staged home waiting to be filled, with the directory that owns it.
async fn staged_target(name: &str) -> (SqliteTarget, tempfile::TempDir) {
    let directory = tempfile::tempdir().expect("staging directory");
    let staging_home = directory.path().join(name);
    std::fs::create_dir_all(&staging_home).expect("staging home");
    let target = SqliteTarget::create(
        SqliteConfig::new_for_testing(staging_home.abs()),
        directory.path().join("final"),
        "migration-provider",
    )
    .await
    .expect("staged databases");
    (target, directory)
}

pub(super) async fn export_phase(
    pool: &Arc<PostgresPool>,
    source: &SqliteSource,
    threads: &[ThreadMetadata],
) {
    // A store that was never activated has no dataset to hand back.
    reset_target(pool).await;
    let (unused, _unused_directory) = staged_target("never-activated").await;
    let refused = Migrator::new(SqliteSource::new(unused.staging().clone()), pool.clone())
        .export(&unused)
        .await;
    unused.close().await;
    assert!(
        matches!(refused, Err(MigrationError::NotActivated)),
        "{refused:?}"
    );

    // Activate a verified import, then write it back out.
    let migrator = Migrator::new(source.clone(), pool.clone());
    let imported = migrator.import().await.expect("import");
    migrator.verify(imported.run_id).await.expect("verify");
    migrator
        .activate(
            imported.run_id,
            ActivationTarget {
                dataset_id: Uuid::new_v4(),
                generation: 2,
            },
        )
        .await
        .expect("activate");

    // An interrupted export keeps its checkpoints and the store stays closed to writers.
    let (staged, _directory) = staged_target("exported").await;
    let staged_source = SqliteSource::new(staged.staging().clone());
    let interrupted = Migrator::new(staged_source.clone(), pool.clone())
        .with_batch_size(2)
        .with_batch_limit(3)
        .export(&staged)
        .await;
    assert!(
        matches!(interrupted, Err(MigrationError::Interrupted)),
        "{interrupted:?}"
    );
    assert!(
        Migrator::new(staged_source.clone(), pool.clone())
            .activation_state()
            .await
            .expect("state")
            .migrating
    );
    let exporter = Migrator::new(staged_source, pool.clone()).with_batch_size(2);
    let exported = exporter.export(&staged).await.expect("resumed export");
    assert!(exported.resumed);
    assert_eq!(exported.domains, imported.domains);
    exporter
        .verify(exported.run_id)
        .await
        .expect("the staged home matches the dataset");
    staged.close().await;

    // The staged home opens with the ordinary runtime and answers like the source did.
    let original = StateRuntime::init(
        SqliteConfig::new_for_testing(source.home().abs()),
        "migration-provider".to_string(),
    )
    .await
    .expect("source runtime");
    let opened = StateRuntime::init(staged.staging().clone(), "migration-provider".to_string())
        .await
        .expect("staged runtime");
    for thread in threads {
        let mut expected = original
            .get_thread(thread.id)
            .await
            .expect("source thread")
            .expect("source thread exists");
        let actual = opened
            .get_thread(thread.id)
            .await
            .expect("staged thread")
            .expect("staged thread exists");
        assert!(actual.rollout_path.starts_with(staged.final_home()));
        expected.rollout_path = actual.rollout_path.clone();
        assert_eq!(actual, expected, "thread {}", thread.id);
        assert_eq!(
            opened
                .thread_goals()
                .get_thread_goal(thread.id)
                .await
                .expect("staged goal"),
            original
                .thread_goals()
                .get_thread_goal(thread.id)
                .await
                .expect("source goal"),
        );
    }

    // A fork's file holds its whole logical history, in the layout the recorder uses.
    let fork = opened
        .get_thread(threads[1].id)
        .await
        .expect("fork")
        .expect("fork exists");
    let staged_file = staged.staged_path(&fork.rollout_path);
    let parent = std::fs::read_to_string(&threads[0].rollout_path).expect("parent rollout");
    let own = std::fs::read_to_string(&threads[1].rollout_path).expect("fork rollout");
    let expected_lines: Vec<&str> = parent
        .lines()
        .take(2)
        .chain(own.lines().filter(|line| !line.is_empty()))
        .collect();
    let written = std::fs::read_to_string(&staged_file).expect("staged rollout");
    assert_eq!(written.lines().collect::<Vec<_>>(), expected_lines);
    assert!(
        fork.rollout_path
            .to_string_lossy()
            .contains(&format!("sessions{0}", std::path::MAIN_SEPARATOR))
    );
    // A thread with no records has no file, like a local thread before its first message.
    let silent = opened
        .get_thread(threads[2].id)
        .await
        .expect("silent")
        .expect("silent exists");
    assert!(!staged.staged_path(&silent.rollout_path).exists());
    opened.close().await;
    original.close().await;
    reset_target(pool).await;
}
