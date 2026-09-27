use super::*;
use pretty_assertions::assert_eq;
use tempfile::tempdir;

#[test]
fn fresh_home_identity_survives_reopen_and_reuses_the_same_claim() {
    let directory = tempdir().unwrap();
    let first = initialize_empty_home(directory.path()).unwrap();
    assert_eq!(load_local_authority(directory.path()).unwrap(), first);
    assert_eq!(initialize_empty_home(directory.path()).unwrap(), first);
}

#[test]
fn missing_activation_marker_requires_fenced_reconciliation() {
    let directory = tempdir().unwrap();
    let first = initialize_empty_home(directory.path()).unwrap();
    std::fs::remove_file(directory.path().join(ACTIVATION_FILE)).unwrap();
    assert!(matches!(
        initialize_empty_home(directory.path()),
        Err(AuthorityError::Blocked(
            "home requires fenced reconciliation"
        ))
    ));
    assert!(matches!(
        load_local_authority(directory.path()),
        Err(AuthorityError::Blocked("authority record missing"))
    ));
    assert_eq!(
        serde_json::from_slice::<LocalIdentity>(
            &std::fs::read(directory.path().join(IDENTITY_FILE)).unwrap()
        )
        .unwrap(),
        first.identity
    );
}

#[test]
fn unknown_version_and_oversized_record_fail_closed() {
    let directory = tempdir().unwrap();
    let mut authority = initialize_empty_home(directory.path()).unwrap();
    authority.identity.format_version += 1;
    std::fs::remove_file(directory.path().join(IDENTITY_FILE)).unwrap();
    write_new(&directory.path().join(IDENTITY_FILE), &authority.identity).unwrap();
    assert!(matches!(
        load_local_authority(directory.path()),
        Err(AuthorityError::Blocked(
            "unsupported authority record version"
        ))
    ));
    std::fs::write(directory.path().join(IDENTITY_FILE), vec![b'x'; 4097]).unwrap();
    assert!(matches!(
        load_local_authority(directory.path()),
        Err(AuthorityError::Blocked("authority record too large"))
    ));
}

#[cfg(unix)]
#[test]
fn linked_record_is_rejected() {
    let directory = tempdir().unwrap();
    initialize_empty_home(directory.path()).unwrap();
    std::fs::remove_file(directory.path().join(ACTIVATION_FILE)).unwrap();
    std::os::unix::fs::symlink(
        directory.path().join(IDENTITY_FILE),
        directory.path().join(ACTIVATION_FILE),
    )
    .unwrap();
    assert!(matches!(
        load_local_authority(directory.path()),
        Err(AuthorityError::Blocked("authority record is a symlink"))
    ));
}

#[test]
fn existing_history_cannot_be_claimed_without_a_fenced_adoption_protocol() {
    let directory = tempdir().unwrap();
    std::fs::write(directory.path().join("state_5.sqlite"), b"existing").unwrap();
    assert!(matches!(
        initialize_empty_home(directory.path()),
        Err(AuthorityError::Blocked(
            "home requires fenced reconciliation"
        ))
    ));
    assert!(!directory.path().join(IDENTITY_FILE).exists());
}

#[test]
fn interrupted_or_conflicting_authority_fails_closed() {
    let directory = tempdir().unwrap();
    let authority = initialize_empty_home(directory.path()).unwrap();
    std::fs::remove_file(directory.path().join(ACTIVATION_FILE)).unwrap();
    assert!(matches!(
        load_local_authority(directory.path()),
        Err(AuthorityError::Blocked("authority record missing"))
    ));

    let mut marker = authority.marker;
    marker.home_id = Uuid::new_v4();
    write_new(&directory.path().join("conflict.json"), &marker).unwrap();
    std::fs::rename(
        directory.path().join("conflict.json"),
        directory.path().join(ACTIVATION_FILE),
    )
    .unwrap();
    assert!(matches!(
        load_local_authority(directory.path()),
        Err(AuthorityError::Blocked("authority records disagree"))
    ));
}

#[test]
fn prior_remote_activation_cannot_reopen_local_authority() {
    let directory = tempdir().unwrap();
    let mut authority = initialize_empty_home(directory.path()).unwrap();
    authority.marker.remote_ever_activated = true;
    std::fs::remove_file(directory.path().join(ACTIVATION_FILE)).unwrap();
    write_new(&directory.path().join(ACTIVATION_FILE), &authority.marker).unwrap();
    assert!(matches!(
        load_local_authority(directory.path()),
        Err(AuthorityError::Blocked(
            "remote authority requires reconciliation"
        ))
    ));
}

#[test]
fn candidate_validation_requires_a_reference_and_ca_path() {
    let credential = CredentialRef::parse("vault_entry_1".to_string()).unwrap();
    let candidate = PostgresCandidate {
        server_name: "postgres.example.test".to_string(),
        port: 5432,
        database: "codex".to_string(),
        namespace: "codex_storage".to_string(),
        credential,
        ca_certificate: std::env::current_dir().unwrap().join("ca.crt"),
    };
    candidate.validate().unwrap();
    let mut invalid = candidate;
    invalid.server_name = "user:secret@postgres.example.test".to_string();
    assert!(invalid.validate().is_err());
    assert!(CredentialRef::parse("password=secret".to_string()).is_err());
}
