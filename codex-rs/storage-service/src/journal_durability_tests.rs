use super::*;
use std::sync::Arc;
use std::sync::Mutex;
fn record(state: OperationState) -> OperationRecord {
    OperationRecord {
        operation_id: Uuid::from_u128(99),
        action: PlanAction::Return,
        plan_digest: "owned".into(),
        state,
        run_id: Some(Uuid::from_u128(100)),
        return_source: Some(ReturnSource {
            dataset_id: Uuid::from_u128(101),
            generation: 2,
            destination: "fixture".into(),
        }),
        created_at_ms: 1,
        updated_at_ms: 2,
        blocker: None,
        copied: vec![],
    }
}
type Fixture = (
    Journal,
    Arc<Mutex<SyncProbe>>,
    Arc<Mutex<Vec<OperationRecord>>>,
);
fn fixture(home: &std::path::Path) -> Fixture {
    let probe = Arc::new(Mutex::new(SyncProbe::default()));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);
    let mut journal = Journal::new(home).observed_by(Arc::new(move |value| {
        sink.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(value.clone())
    }));
    journal.sync_probe = Some(Arc::clone(&probe));
    (journal, probe, seen)
}
#[test]
fn ancestor_failure_no_record_or_observer_and_retry_reflushes_existing_directories() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("fresh/nested/home");
    let (journal, probe, seen) = fixture(&home);
    probe.lock().unwrap().fail = Some("ancestor-parent");
    let planned = record(OperationState::Planned);
    assert!(journal.create(&planned).is_err());
    assert!(journal.directory.is_dir());
    assert!(!journal.path(planned.operation_id).exists());
    assert!(seen.lock().unwrap().is_empty());
    probe.lock().unwrap().events.clear();
    journal.create(&planned).unwrap();
    let events = probe.lock().unwrap().events.clone();
    let expected: Vec<_> = std::path::absolute(&journal.directory)
        .unwrap()
        .ancestors()
        .skip(1)
        .map(|p| ("ancestor-parent", p.to_path_buf()))
        .collect();
    assert_eq!(&events[..expected.len()], expected.as_slice());
    assert_eq!(events[expected.len()].0, "record-file");
    assert_eq!(events[expected.len() + 1].0, "journal-parent");
    assert_eq!(*seen.lock().unwrap(), vec![planned.clone()]);
    assert_eq!(
        Journal::new(&home).read(planned.operation_id).unwrap(),
        Some(planned)
    );
}
#[test]
fn create_file_and_parent_failures_never_announce_success() {
    for phase in ["record-file", "journal-parent"] {
        let home = tempfile::tempdir().unwrap();
        let (journal, probe, seen) = fixture(home.path());
        probe.lock().unwrap().fail = Some(phase);
        let planned = record(OperationState::Planned);
        assert!(journal.create(&planned).is_err());
        assert!(seen.lock().unwrap().is_empty());
        // A visible complete record is evidence, not a durable success acknowledgment.
        assert_eq!(
            journal.read(planned.operation_id).unwrap(),
            Some(planned.clone())
        );
        assert!(journal.create(&planned).is_err());
        journal.update(&planned).unwrap();
        assert_eq!(*seen.lock().unwrap(), vec![planned]);
    }
}
#[test]
fn update_failure_preserves_old_before_rename_and_visible_new_after_rename_without_ack() {
    for phase in ["update-file", "journal-parent"] {
        let home = tempfile::tempdir().unwrap();
        let (journal, probe, seen) = fixture(home.path());
        let planned = record(OperationState::Planned);
        let ready = record(OperationState::Ready);
        journal.create(&planned).unwrap();
        seen.lock().unwrap().clear();
        probe.lock().unwrap().fail = Some(phase);
        assert!(journal.update(&ready).is_err());
        assert!(seen.lock().unwrap().is_empty());
        let expected = if phase == "update-file" {
            &planned
        } else {
            &ready
        };
        assert_eq!(
            journal.read(ready.operation_id).unwrap().as_ref(),
            Some(expected)
        );
        assert_eq!(
            std::fs::read_dir(&journal.directory)
                .unwrap()
                .any(|entry| entry
                    .unwrap()
                    .path()
                    .extension()
                    .is_some_and(|value| value == "tmp")),
            phase == "update-file"
        );
        journal.update(&ready).unwrap();
        assert_eq!(*seen.lock().unwrap(), vec![ready.clone()]);
        assert_eq!(
            Journal::new(home.path()).list_checked().unwrap(),
            vec![ready]
        );
    }
}
