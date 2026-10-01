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
