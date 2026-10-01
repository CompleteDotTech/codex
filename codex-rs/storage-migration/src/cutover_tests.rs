//! Failure injection at every boundary of the cutover: each case ends with exactly one
//! authoritative backend and no data deleted.

use super::tests::reset_target;
use super::*;
use crate::Cutover;
use crate::CutoverError;
use crate::RecoveryOutcome;
use codex_storage_authority::ActiveBackend;
use codex_storage_authority::AuthorityError;
use codex_storage_authority::adopt_quiesced_home;
use codex_storage_authority::load_authority;
use codex_storage_authority::read_cutover;
use std::path::Path;
use uuid::Uuid;

const AUTHORITY_FILES: [&str; 3] = [
    "storage-identity.json",
    "storage-activation.json",
    "storage-cutover.json",
];

/// Start a case from a fresh local authority and a verified copy waiting in PostgreSQL.
async fn verified_run(
    pool: &Arc<PostgresPool>,
    source: &SqliteSource,
    home: &Path,
) -> (Cutover, Uuid) {
    reset_target(pool).await;
    for file in AUTHORITY_FILES {
        let _ = std::fs::remove_file(home.join(file));
    }
    adopt_quiesced_home(home).expect("adopt the quiesced home");
    let migrator = Migrator::new(source.clone(), pool.clone());
    let summary = migrator.import().await.expect("import");
    migrator.verify(summary.run_id).await.expect("verify");
    let cutover = Cutover::new(
        home.to_path_buf(),
        Migrator::new(source.clone(), pool.clone()),
    );
    (cutover, summary.run_id)
}

fn local_generation(home: &Path) -> u64 {
    load_authority(home)
        .expect("settled authority")
        .identity
        .generation
}

fn backend(home: &Path) -> ActiveBackend {
    load_authority(home)
        .expect("settled authority")
        .marker
        .active_backend
}

pub(super) async fn cutover_phase(pool: &Arc<PostgresPool>, source: &SqliteSource, home: &Path) {
    let rollout_bytes = std::fs::read(home.join("rollouts/thread-0.jsonl")).expect("rollout");

    // The whole protocol: the local records and the destination agree on the next generation.
    let (cutover, run_id) = verified_run(pool, source, home).await;
    let moved = cutover.execute(run_id).await.expect("cutover");
    assert_eq!(moved.marker.active_backend, ActiveBackend::Remote);
    let status = cutover.status().await.expect("status");
    assert_eq!(status.intent, None);
    assert_eq!(status.local_generation, Some(2));
    assert_eq!(status.remote.generation, 2);
    assert_eq!(status.remote.dataset_id, Some(moved.identity.dataset_id));
    assert!(!status.remote.migrating);
    // A finished cutover cannot be cancelled; leaving PostgreSQL needs the reverse migration.
    assert!(matches!(
        cutover.abort().await,
        Err(CutoverError::AlreadyActivated)
    ));
    assert!(matches!(cutover.recover().await, Ok(RecoveryOutcome::Idle)));

    // Crash after the intent: the destination never published, so recovery undoes the intent
    // and the source stays authoritative. The same verified copy then cuts over normally.
    let (cutover, run_id) = verified_run(pool, source, home).await;
    let intent = cutover.prepare(run_id).expect("prepare");
    assert!(matches!(
        cutover.recover().await,
        Ok(RecoveryOutcome::RolledBack)
    ));
    assert_eq!(read_cutover(home).expect("intent"), None);
    assert_eq!(backend(home), ActiveBackend::Local);
    assert_eq!(local_generation(home), intent.from_generation);
    cutover
        .execute(run_id)
        .await
        .expect("cutover after rollback");
    assert_eq!(backend(home), ActiveBackend::Remote);

    // Lost acknowledgement: the destination published but the host never saw the answer and
    // never touched its own records. Recovery rolls forward, and repeating it is harmless.
    let (cutover, run_id) = verified_run(pool, source, home).await;
    let intent = cutover.prepare(run_id).expect("prepare");
    cutover.publish(&intent).await.expect("publish");
    assert!(matches!(
        load_authority(home),
        Err(AuthorityError::Blocked("cutover in progress"))
    ));
    assert!(matches!(
        cutover.recover().await,
        Ok(RecoveryOutcome::RolledForward { generation: 2 })
    ));
    assert_eq!(backend(home), ActiveBackend::Remote);
    assert!(matches!(cutover.recover().await, Ok(RecoveryOutcome::Idle)));

    // Configuration write failure after the destination published: recovery cannot finish
    // while the write keeps failing, never reports a settled state, and finishes once it works.
    let (cutover, run_id) = verified_run(pool, source, home).await;
    let intent = cutover.prepare(run_id).expect("prepare");
    cutover.publish(&intent).await.expect("publish");
    let blocker = home.join("storage-identity.json.tmp");
    std::fs::create_dir(&blocker).expect("block the write");
    assert!(cutover.finish(&intent).is_err());
    assert!(cutover.recover().await.is_err());
    assert_eq!(read_cutover(home).expect("intent"), Some(intent.clone()));
    std::fs::remove_dir(&blocker).expect("unblock the write");
    assert!(matches!(
        cutover.recover().await,
        Ok(RecoveryOutcome::RolledForward { generation: 2 })
    ));

    // Another client published a different generation: nothing is guessed, the intent stays.
    let (cutover, run_id) = verified_run(pool, source, home).await;
    let intent = cutover.prepare(run_id).expect("prepare");
    let migrator = Migrator::new(source.clone(), pool.clone());
    migrator
        .activate(
            run_id,
            ActivationTarget {
                dataset_id: Uuid::new_v4(),
                generation: 7,
            },
        )
        .await
        .expect("foreign activation");
    assert!(matches!(
        cutover.recover().await,
        Err(CutoverError::Conflict)
    ));
    assert_eq!(read_cutover(home).expect("intent"), Some(intent));
    assert_eq!(
        std::fs::read(home.join("rollouts/thread-0.jsonl")).expect("rollout"),
        rollout_bytes
    );

    // Cancelling before publication keeps the source, fences the partial copy, and a later
    // attempt with the same source resumes the abandoned run.
    let (cutover, run_id) = verified_run(pool, source, home).await;
    cutover.prepare(run_id).expect("prepare");
    assert!(matches!(
        cutover.abort().await,
        Ok(RecoveryOutcome::RolledBack)
    ));
    assert_eq!(backend(home), ActiveBackend::Local);
    let held = cutover.status().await.expect("status").remote;
    assert!(held.migrating);
    let resumed = Migrator::new(source.clone(), pool.clone())
        .import()
        .await
        .expect("resume the abandoned run");
    assert!(resumed.resumed);
    assert_eq!(resumed.run_id, run_id);

    // A run that was never verified cannot take authority, and the refusal undoes the intent.
    let (cutover, run_id) = verified_run(pool, source, home).await;
    cutover.abort().await.expect("clear");
    reset_target(pool).await;
    let unverified = Migrator::new(source.clone(), pool.clone())
        .import()
        .await
        .expect("import without verification");
    assert_ne!(unverified.run_id, run_id);
    let refused = cutover.execute(unverified.run_id).await;
    assert!(
        matches!(
            refused,
            Err(CutoverError::Destination(MigrationError::NotVerified))
        ),
        "{refused:?}"
    );
    assert_eq!(read_cutover(home).expect("intent"), None);
    assert_eq!(backend(home), ActiveBackend::Local);

    // None of this touched the source data.
    assert_eq!(
        std::fs::read(home.join("rollouts/thread-0.jsonl")).expect("rollout"),
        rollout_bytes
    );
    reset_target(pool).await;
}
