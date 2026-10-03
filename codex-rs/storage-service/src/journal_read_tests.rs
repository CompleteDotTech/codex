use super::*;
fn operation() -> OperationRecord {
    OperationRecord {
        operation_id: Uuid::from_u128(71),
        action: PlanAction::Return,
        plan_digest: "owned".into(),
        state: OperationState::Planned,
        run_id: None,
        return_source: None,
        created_at_ms: 1,
        updated_at_ms: 2,
        blocker: None,
        copied: vec![],
    }
}
#[test]
fn oversized_sparse_and_wrong_owner_records_are_preserved_and_refused() {
    let home = tempfile::tempdir().unwrap();
    let journal = Journal::new(home.path());
    let value = operation();
    journal.create(&value).unwrap();
    let path = journal.path(value.operation_id);
    let original = std::fs::read(&path).unwrap();
    std::fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .set_len(RECORD_BYTE_LIMIT as u64 + 1)
        .unwrap();
    assert_eq!(
        journal.read(value.operation_id).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
    assert!(journal.list_checked().is_err());
    assert_eq!(
        std::fs::metadata(&path).unwrap().len(),
        RECORD_BYTE_LIMIT as u64 + 1
    );
    std::fs::write(&path, &original).unwrap();
    let wrong = journal.path(Uuid::from_u128(72));
    std::fs::rename(&path, &wrong).unwrap();
    assert!(journal.read(Uuid::from_u128(72)).is_err());
    assert!(journal.list_checked().is_err());
    assert_eq!(std::fs::read(wrong).unwrap(), original);
}
#[test]
fn legacy_absence_and_invalid_namespace_are_distinct() {
    let home = tempfile::tempdir().unwrap();
    let journal = Journal::new(home.path());
    let value = operation();
    assert!(journal.read(value.operation_id).unwrap().is_none());
    journal.create(&value).unwrap();
    let bytes = std::fs::read(journal.path(value.operation_id)).unwrap();
    assert_eq!(
        journal.read(value.operation_id).unwrap(),
        Some(value.clone())
    );
    assert_eq!(
        std::fs::read(journal.path(value.operation_id)).unwrap(),
        bytes
    );
    std::fs::remove_file(journal.path(value.operation_id)).unwrap();
    std::fs::remove_dir(&journal.directory).unwrap();
    std::fs::write(&journal.directory, b"not directory").unwrap();
    assert!(journal.read(value.operation_id).is_err());
    assert!(journal.list_checked().is_err());
}
#[cfg(unix)]
#[test]
fn atomic_nofollow_nonblocking_refuses_symlink_hardlink_and_fifo() {
    use std::os::unix::ffi::OsStrExt;
    let home = tempfile::tempdir().unwrap();
    let foreign = tempfile::tempdir().unwrap();
    let journal = Journal::new(home.path());
    let value = operation();
    std::fs::create_dir(&journal.directory).unwrap();
    let path = journal.path(value.operation_id);
    let external = foreign.path().join("external");
    let original = serde_json::to_vec(&value).unwrap();
    std::fs::write(&external, &original).unwrap();
    std::os::unix::fs::symlink(&external, &path).unwrap();
    assert!(journal.read(value.operation_id).is_err());
    std::fs::remove_file(&path).unwrap();
    std::fs::hard_link(&external, &path).unwrap();
    assert!(journal.read(value.operation_id).is_err());
    std::fs::remove_file(&path).unwrap();
    let native = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(native.as_ptr(), 0o600) }, 0);
    // Direct helper bypasses list prechecks and performs the actual nonblocking open.
    assert!(read_record(&path, value.operation_id).is_err());
    assert!(journal.read(value.operation_id).is_err());
    assert_eq!(std::fs::read(&external).unwrap(), original);
    assert!(
        !std::fs::symlink_metadata(&path)
            .unwrap()
            .file_type()
            .is_file()
    );
}

#[test]
fn checked_entry_limit_accepts_boundary_and_refuses_next_without_omission() {
    let home = tempfile::tempdir().unwrap();
    let journal = Journal::new(home.path());
    std::fs::create_dir(&journal.directory).unwrap();
    for index in 0..RECORD_COUNT_LIMIT {
        std::fs::File::create(journal.directory.join(format!("ignored-{index}.lock"))).unwrap();
    }
    assert!(journal.list_checked().unwrap().is_empty());
    let extra = journal.directory.join("over-boundary.lock");
    std::fs::File::create(&extra).unwrap();
    assert!(
        journal
            .list_checked()
            .unwrap_err()
            .to_string()
            .contains("entry limit")
    );
    assert!(extra.exists());
    assert_eq!(
        std::fs::read_dir(&journal.directory).unwrap().count(),
        RECORD_COUNT_LIMIT + 1
    );
}
#[test]
fn aggregate_exact_boundary_accepts_and_next_record_refuses_before_decode() {
    let home = tempfile::tempdir().unwrap();
    let journal = Journal::new(home.path());
    std::fs::create_dir(&journal.directory).unwrap();
    for index in 0..16 {
        let mut value = operation();
        value.operation_id = Uuid::from_u128(1000 + index);
        let fixed = serde_json::to_vec(&value).unwrap().len();
        value
            .plan_digest
            .push_str(&"x".repeat(RECORD_BYTE_LIMIT - fixed));
        let bytes = serde_json::to_vec(&value).unwrap();
        assert_eq!(bytes.len(), RECORD_BYTE_LIMIT);
        std::fs::write(journal.path(value.operation_id), bytes).unwrap();
    }
    assert_eq!(journal.list_checked().unwrap().len(), 16);
    let mut seventeenth = operation();
    seventeenth.operation_id = Uuid::from_u128(9999);
    let extra = journal.path(seventeenth.operation_id);
    let original = serde_json::to_vec(&seventeenth).unwrap();
    std::fs::write(&extra, &original).unwrap();
    // Every record is valid: only aggregate byte admission can reject this list.
    let error = journal.list_checked().unwrap_err();
    assert!(error.to_string().contains("byte limit"));
    assert_eq!(std::fs::read(&extra).unwrap(), original);
    assert_eq!(std::fs::read_dir(&journal.directory).unwrap().count(), 17);
    // Independent invalid JSON with zero remaining budget proves predecode refusal.
    let invalid = journal.path(Uuid::from_u128(10000));
    std::fs::write(&invalid, b"deliberately invalid JSON").unwrap();
    let error = read_record_bounded(&invalid, Uuid::from_u128(10000), 0).unwrap_err();
    assert!(error.to_string().contains("byte limit"));
    assert_eq!(
        std::fs::read(invalid).unwrap(),
        b"deliberately invalid JSON"
    );
}
#[cfg(unix)]
#[test]
fn actual_namespace_substitution_after_open_refuses_held_old_record() {
    let home = tempfile::tempdir().unwrap();
    let journal = Journal::new(home.path());
    let original = operation();
    journal.create(&original).unwrap();
    let path = journal.path(original.operation_id);
    let held = open_record(&path).unwrap();
    let old = path.with_extension("held-original");
    std::fs::rename(&path, &old).unwrap();
    let mut replacement = original.clone();
    replacement.state = OperationState::Cancelled;
    std::fs::write(&path, serde_json::to_vec(&replacement).unwrap()).unwrap();
    assert!(
        read_open_record(&path, original.operation_id, held, RECORD_BYTE_LIMIT)
            .unwrap_err()
            .to_string()
            .contains("identity changed")
    );
    assert_eq!(
        serde_json::from_slice::<OperationRecord>(&std::fs::read(old).unwrap()).unwrap(),
        original
    );
    assert_eq!(
        journal.read(replacement.operation_id).unwrap(),
        Some(replacement)
    );
}
