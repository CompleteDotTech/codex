use super::*;
use pretty_assertions::assert_eq;
use std::sync::Arc;
use std::sync::Mutex;

fn record(state: OperationState, copied: u64) -> OperationRecord {
    OperationRecord {
        operation_id: Uuid::from_u128(7),
        action: PlanAction::Migrate,
        plan_digest: "digest".to_string(),
        state,
        run_id: None,
        return_source: None,
        created_at_ms: 1,
        updated_at_ms: 2,
        blocker: None,
        copied: vec![("threads".to_string(), copied)],
    }
}

#[test]
fn the_observer_sees_each_durable_change_in_order() {
    let home = tempfile::tempdir().expect("home");
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);
    let journal = Journal::new(home.path()).observed_by(Arc::new(move |record| {
        sink.lock().expect("sink").push(record.clone());
    }));

    let planned = record(OperationState::Planned, 0);
    let copying = record(OperationState::Copying, 3);
    journal.create(&planned).expect("create");
    journal.update(&copying).expect("update");

    assert_eq!(*seen.lock().expect("seen"), vec![planned, copying.clone()]);
    assert_eq!(
        journal.read(copying.operation_id).expect("read"),
        Some(copying)
    );
}

#[test]
fn a_rejected_write_is_not_reported() {
    let home = tempfile::tempdir().expect("home");
    let seen = Arc::new(Mutex::new(0usize));
    let sink = Arc::clone(&seen);
    let journal = Journal::new(home.path()).observed_by(Arc::new(move |_| {
        *sink.lock().expect("sink") += 1;
    }));

    journal
        .create(&record(OperationState::Planned, 0))
        .expect("create");
    journal
        .create(&record(OperationState::Planned, 0))
        .expect_err("the id is already taken");

    assert_eq!(*seen.lock().expect("seen"), 1);
}

#[test]
fn return_identity_survives_restart_and_export_cancel_exclude_each_other() {
    let home = tempfile::tempdir().expect("home");
    let journal = Journal::new(home.path());
    let mut planned = record(OperationState::Planned, 0);
    planned.action = PlanAction::Return;
    planned.run_id = Some(Uuid::new_v4());
    planned.return_source = Some(ReturnSource {
        dataset_id: Uuid::new_v4(),
        generation: 2,
        destination: "host:5432/database/namespace".into(),
    });
    journal
        .create(&planned)
        .expect("persist before any remote fence");
    let restarted = Journal::new(home.path());
    assert_eq!(
        restarted.read(planned.operation_id).expect("restart"),
        Some(planned.clone())
    );
    let export = journal
        .claim_return(planned.operation_id)
        .expect("export owner");
    assert!(restarted.claim_return(planned.operation_id).is_err());
    drop(export);
    let cancel = restarted
        .claim_return(planned.operation_id)
        .expect("cancel after exporter exits");
    drop(cancel);
    // A failed ownership journal creation cannot announce a usable export receipt.
    assert!(journal.create(&planned).is_err());
    assert_eq!(
        restarted.read(planned.operation_id).expect("unchanged"),
        Some(planned)
    );
}

#[test]
fn recovery_does_not_hide_a_corrupt_record_beside_a_valid_operation() {
    let home = tempfile::tempdir().expect("home");
    let journal = Journal::new(home.path());
    let operation = record(OperationState::Committing, 3);
    journal.create(&operation).expect("create");
    let corrupt = journal.path(Uuid::from_u128(8));
    std::fs::write(&corrupt, b"credential-canary: truncated journal").expect("corrupt");
    let error = journal
        .list_checked()
        .expect_err("ownership evidence is incomplete");
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    assert!(!error.to_string().contains("credential-canary"));
    std::fs::remove_file(corrupt).expect("remove injected corruption");
    assert_eq!(
        journal.list_checked().expect("complete evidence"),
        vec![operation]
    );
}

#[test]
fn recovery_refuses_a_record_under_another_operations_name() {
    let home = tempfile::tempdir().expect("home");
    let journal = Journal::new(home.path());
    let operation = record(OperationState::Committing, 3);
    journal.create(&operation).expect("create");
    std::fs::rename(
        journal.path(operation.operation_id),
        journal.path(Uuid::from_u128(8)),
    )
    .expect("misidentify record");
    assert_eq!(
        journal.list_checked().expect_err("wrong owner").kind(),
        std::io::ErrorKind::InvalidData
    );
}

#[test]
fn absent_journal_and_invalid_journal_are_distinct_recovery_states() {
    let home = tempfile::tempdir().expect("home");
    let journal = Journal::new(home.path());
    assert!(journal.list_checked().expect("never created").is_empty());
    std::fs::write(&journal.directory, b"not a directory").expect("invalid namespace");
    assert_eq!(
        journal
            .list_checked()
            .expect_err("cannot infer idle")
            .kind(),
        std::io::ErrorKind::InvalidData
    );
}

#[test]
fn recovery_refuses_a_directory_in_place_of_an_operation_record() {
    let home = tempfile::tempdir().expect("home");
    let journal = Journal::new(home.path());
    std::fs::create_dir_all(journal.path(Uuid::from_u128(8))).expect("invalid record");
    assert_eq!(
        journal.list_checked().expect_err("not a record").kind(),
        std::io::ErrorKind::InvalidData
    );
}

#[test]
fn recovery_refuses_uppercase_records_without_hiding_or_rewriting_them() {
    let home = tempfile::tempdir().expect("home");
    let journal = Journal::new(home.path());
    let operation = record(OperationState::Committing, 3);
    journal.create(&operation).expect("create");
    std::fs::rename(
        journal.path(operation.operation_id),
        journal.path(operation.operation_id).with_extension("JSON"),
    )
    .expect("change extension spelling");
    assert_eq!(
        journal
            .list_checked()
            .expect_err("noncanonical owner is not absent")
            .kind(),
        std::io::ErrorKind::InvalidData
    );
    let retained = std::fs::read(journal.path(operation.operation_id).with_extension("JSON"))
        .expect("record preserved");
    assert_eq!(
        serde_json::from_slice::<OperationRecord>(&retained).expect("valid record"),
        operation
    );
    assert_eq!(
        std::fs::read_dir(&journal.directory)
            .expect("journal")
            .count(),
        1
    );
}

#[test]
fn recovery_refuses_duplicate_owners_under_alternate_uuid_spellings() {
    let home = tempfile::tempdir().expect("home");
    let journal = Journal::new(home.path());
    let operation = record(OperationState::Committing, 3);
    journal.create(&operation).expect("create");
    let duplicate = journal
        .directory
        .join(format!("{}.json", operation.operation_id.simple()));
    std::fs::copy(journal.path(operation.operation_id), duplicate).expect("duplicate owner");
    assert_eq!(
        journal
            .list_checked()
            .expect_err("ambiguous ownership")
            .kind(),
        std::io::ErrorKind::InvalidData
    );
}

#[cfg(unix)]
#[test]
fn recovery_refuses_redirected_journal_and_record_paths() {
    let home = tempfile::tempdir().expect("home");
    let foreign = tempfile::tempdir().expect("foreign");
    let journal = Journal::new(home.path());
    std::os::unix::fs::symlink(foreign.path(), &journal.directory).expect("redirect namespace");
    assert!(journal.list_checked().is_err());
    std::fs::remove_file(&journal.directory).expect("remove link");
    std::fs::create_dir(&journal.directory).expect("own namespace");
    let operation = record(OperationState::Committing, 3);
    let external = foreign.path().join("record");
    std::fs::write(
        &external,
        serde_json::to_vec(&operation).expect("serialize"),
    )
    .expect("foreign record");
    std::os::unix::fs::symlink(&external, journal.path(operation.operation_id))
        .expect("redirect record");
    assert!(journal.list_checked().is_err());
    assert!(external.is_file());
}
