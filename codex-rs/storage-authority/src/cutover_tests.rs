use super::*;
use crate::ActiveBackend;
use crate::initialize_empty_home;
use crate::load_local_authority;
use pretty_assertions::assert_eq;
use tempfile::tempdir;

fn remote_run() -> Uuid {
    Uuid::new_v4()
}

#[test]
fn cutover_moves_both_records_to_the_next_generation() {
    let directory = tempdir().unwrap();
    let first = initialize_empty_home(directory.path()).unwrap();
    let intent = begin_cutover(directory.path(), remote_run(), ActiveBackend::Remote).unwrap();
    assert_eq!(
        intent,
        CutoverIntent {
            format_version: FORMAT_VERSION,
            run_id: intent.run_id,
            dataset_id: first.identity.dataset_id,
            from_generation: 1,
            to_generation: 2,
            target: ActiveBackend::Remote,
        }
    );

    // While the intent exists no caller may treat the home as settled.
    assert!(matches!(
        load_local_authority(directory.path()),
        Err(AuthorityError::Blocked("cutover in progress"))
    ));
    assert!(matches!(
        load_authority(directory.path()),
        Err(AuthorityError::Blocked("cutover in progress"))
    ));
    assert!(matches!(
        begin_cutover(directory.path(), remote_run(), ActiveBackend::Remote),
        Err(AuthorityError::Blocked("cutover already in progress"))
    ));

    let moved = complete_cutover(directory.path(), &intent).unwrap();
    assert_eq!(moved.identity.generation, 2);
    assert_eq!(
        moved.marker,
        ActivationMarker {
            format_version: FORMAT_VERSION,
            dataset_id: first.identity.dataset_id,
            instance_id: first.identity.instance_id,
            home_id: first.identity.home_id,
            generation: 2,
            remote_ever_activated: true,
            active_backend: ActiveBackend::Remote,
        }
    );
    assert_eq!(read_cutover(directory.path()).unwrap(), None);
    assert_eq!(load_authority(directory.path()).unwrap(), moved);
    // Callers that only understand local homes keep refusing a remote one.
    assert!(matches!(
        load_local_authority(directory.path()),
        Err(AuthorityError::Blocked(
            "remote authority requires reconciliation"
        ))
    ));
    assert!(matches!(
        begin_cutover(directory.path(), remote_run(), ActiveBackend::Remote),
        Err(AuthorityError::Blocked("backend is already authoritative"))
    ));
}

#[test]
fn completing_twice_is_harmless() {
    let directory = tempdir().unwrap();
    initialize_empty_home(directory.path()).unwrap();
    let intent = begin_cutover(directory.path(), remote_run(), ActiveBackend::Remote).unwrap();
    let first = complete_cutover(directory.path(), &intent).unwrap();
    assert_eq!(complete_cutover(directory.path(), &intent).unwrap(), first);
}

#[test]
fn a_crash_between_the_two_records_rolls_forward() {
    let directory = tempdir().unwrap();
    let before = initialize_empty_home(directory.path()).unwrap();
    let intent = begin_cutover(directory.path(), remote_run(), ActiveBackend::Remote).unwrap();
    // Only the identity record moved before the process died.
    replace_record(
        &directory.path().join(IDENTITY_FILE),
        &LocalIdentity {
            generation: intent.to_generation,
            ..before.identity
        },
    )
    .unwrap();
    assert!(matches!(
        abandon_cutover(directory.path(), &intent),
        Err(AuthorityError::Blocked("cutover already moved the records"))
    ));
    let moved = complete_cutover(directory.path(), &intent).unwrap();
    assert_eq!(moved.marker.generation, 2);
    assert_eq!(moved.marker.active_backend, ActiveBackend::Remote);
}

#[test]
fn a_failed_record_write_leaves_the_intent_for_recovery() {
    let directory = tempdir().unwrap();
    initialize_empty_home(directory.path()).unwrap();
    let intent = begin_cutover(directory.path(), remote_run(), ActiveBackend::Remote).unwrap();
    // A directory where the temporary file belongs makes the configuration write fail.
    let blocker = directory.path().join(format!("{IDENTITY_FILE}.tmp"));
    std::fs::create_dir(&blocker).unwrap();
    assert!(complete_cutover(directory.path(), &intent).is_err());
    assert_eq!(
        read_cutover(directory.path()).unwrap(),
        Some(intent.clone())
    );
    std::fs::remove_dir(&blocker).unwrap();
    let moved = complete_cutover(directory.path(), &intent).unwrap();
    assert_eq!(moved.identity.generation, 2);
}

#[test]
fn abandoning_before_anything_moved_restores_the_local_home() {
    let directory = tempdir().unwrap();
    let before = initialize_empty_home(directory.path()).unwrap();
    let intent = begin_cutover(directory.path(), remote_run(), ActiveBackend::Remote).unwrap();
    abandon_cutover(directory.path(), &intent).unwrap();
    assert_eq!(read_cutover(directory.path()).unwrap(), None);
    assert_eq!(load_local_authority(directory.path()).unwrap(), before);
    // The same intent can be abandoned again and a new cutover can start.
    abandon_cutover(directory.path(), &intent).unwrap();
    begin_cutover(directory.path(), remote_run(), ActiveBackend::Remote).unwrap();
}

#[test]
fn an_intent_for_another_dataset_is_refused() {
    let directory = tempdir().unwrap();
    initialize_empty_home(directory.path()).unwrap();
    let mut intent = begin_cutover(directory.path(), remote_run(), ActiveBackend::Remote).unwrap();
    intent.dataset_id = Uuid::new_v4();
    assert!(matches!(
        complete_cutover(directory.path(), &intent),
        Err(AuthorityError::Blocked(
            "cutover belongs to another dataset"
        ))
    ));
}

#[test]
fn authority_state_classifies_every_stage_of_a_home() {
    let directory = tempdir().unwrap();
    assert_eq!(
        authority_state(directory.path()).unwrap(),
        AuthorityState::Unmanaged
    );
    let local = initialize_empty_home(directory.path()).unwrap();
    assert_eq!(
        authority_state(directory.path()).unwrap(),
        AuthorityState::Local(local)
    );
    let intent = begin_cutover(directory.path(), remote_run(), ActiveBackend::Remote).unwrap();
    assert_eq!(
        authority_state(directory.path()).unwrap(),
        AuthorityState::CutoverInProgress(intent.clone())
    );
    let moved = complete_cutover(directory.path(), &intent).unwrap();
    assert_eq!(
        authority_state(directory.path()).unwrap(),
        AuthorityState::Remote(moved)
    );
    // One record without the other is never interpreted.
    std::fs::remove_file(directory.path().join(ACTIVATION_FILE)).unwrap();
    assert!(authority_state(directory.path()).is_err());
}
