use super::*;
use pretty_assertions::assert_eq;

#[cfg(unix)]
#[test]
fn preopen_fifo_without_writer_is_refused_and_preserved() -> io::Result<()> {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::FileTypeExt;
    use std::os::unix::fs::MetadataExt;
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("receipt");
    let retained = directory.path().join("retained");
    std::fs::write(&path, b"owned")?;
    let owned_id = rollout_file_identity_from_handle(&std::fs::File::open(&path)?)?;
    let fifo_id = std::cell::Cell::new(None);
    let native = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    assert!(
        read_at_boundaries(
            &path,
            ExpectedIdentity::Any,
            || {
                std::fs::rename(&path, &retained)?;
                // SAFETY: native is a live NUL-terminated path owned by this test.
                if unsafe {
                    libc::mkfifo(native.as_ptr(), /*mode*/ 0o600)
                } != 0
                {
                    return Err(io::Error::last_os_error());
                }
                let metadata = std::fs::symlink_metadata(&path)?;
                fifo_id.set(Some((metadata.dev(), metadata.ino())));
                Ok(())
            },
            || Ok(())
        )
        .is_err()
    );
    let metadata = std::fs::symlink_metadata(&path)?;
    assert!(metadata.file_type().is_fifo());
    assert_eq!(Some((metadata.dev(), metadata.ino())), fifo_id.get());
    assert_eq!(
        rollout_file_identity_from_handle(&std::fs::File::open(&retained)?)?,
        owned_id
    );
    assert_eq!(std::fs::read(&retained)?, b"owned");
    Ok(())
}

#[test]
fn complete_receipt_read_and_growth_refusal_preserve_bytes() -> io::Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("receipt");
    std::fs::write(&path, b"owned receipt\n")?;
    let identity = rollout_file_identity_from_handle(&std::fs::File::open(&path)?)?;
    assert_eq!(read_owned(&path, identity)?, b"owned receipt\n");
    let hit = std::cell::Cell::new(0);
    let oversized = vec![b'x'; 5000];
    assert!(
        read_at_boundaries(
            &path,
            ExpectedIdentity::Owned(identity),
            || Ok(()),
            || {
                hit.set(hit.get() + 1);
                std::fs::write(&path, &oversized)
            }
        )
        .is_err()
    );
    assert_eq!(hit.get(), 1);
    assert_eq!(std::fs::read(&path)?, oversized);
    assert_eq!(
        rollout_file_identity_from_handle(&std::fs::File::open(&path)?)?,
        identity
    );
    Ok(())
}

#[test]
fn snapshot_namespace_replacement_preserves_both_files() -> io::Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("receipt");
    let retained = directory.path().join("retained");
    std::fs::write(&path, b"same bytes")?;
    let identity = rollout_file_identity_from_handle(&std::fs::File::open(&path)?)?;
    assert!(
        read_at_boundaries(
            &path,
            ExpectedIdentity::Owned(identity),
            || Ok(()),
            || {
                std::fs::rename(&path, &retained)?;
                std::fs::write(&path, b"same bytes")
            }
        )
        .is_err()
    );
    assert_eq!(std::fs::read(&retained)?, b"same bytes");
    assert_eq!(std::fs::read(&path)?, b"same bytes");
    assert_eq!(
        rollout_file_identity_from_handle(&std::fs::File::open(&retained)?)?,
        identity
    );
    assert_ne!(
        rollout_file_identity_from_handle(&std::fs::File::open(&path)?)?,
        identity
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn preopen_symlink_is_refused_and_foreign_target_retained() -> io::Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("receipt");
    let retained = directory.path().join("retained");
    let foreign = directory.path().join("foreign");
    std::fs::write(&path, b"owned")?;
    std::fs::write(&foreign, b"foreign")?;
    let owned_id = rollout_file_identity_from_handle(&std::fs::File::open(&path)?)?;
    let foreign_id = rollout_file_identity_from_handle(&std::fs::File::open(&foreign)?)?;
    assert!(
        read_at_boundaries(
            &path,
            ExpectedIdentity::Any,
            || {
                std::fs::rename(&path, &retained)?;
                std::os::unix::fs::symlink(&foreign, &path)
            },
            || Ok(())
        )
        .is_err()
    );
    assert_eq!(std::fs::read_link(&path)?, foreign);
    assert_eq!(std::fs::read(&retained)?, b"owned");
    assert_eq!(std::fs::read(&foreign)?, b"foreign");
    assert_eq!(
        rollout_file_identity_from_handle(&std::fs::File::open(&retained)?)?,
        owned_id
    );
    assert_eq!(
        rollout_file_identity_from_handle(&std::fs::File::open(&foreign)?)?,
        foreign_id
    );
    Ok(())
}

#[test]
fn bare_authority_rejects_unknown_nested_fields_and_duplicates() -> io::Result<()> {
    use super::super::RolloutMoveIntent;
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("receipt");
    std::fs::write(&path, b"owned")?;
    let identity = rollout_file_identity_from_handle(&std::fs::File::open(&path)?)?;
    let intent = RolloutMoveIntent {
        source: path.clone(),
        destination: path.clone(),
        source_len: 5,
        source_modified: std::time::UNIX_EPOCH,
        source_id: identity,
        source_digest: [0; 32],
        stage_path: path.clone(),
        stage_id: identity,
        stage_digest: [0; 32],
        quarantine_path: path.clone(),
        receipt_publication: None,
    };
    let original = serde_json::to_vec(&intent).map_err(io::Error::other)?;
    assert!(serde_json::from_slice::<RolloutMoveIntent>(&original).is_ok());
    for field in ["source_id", "stage_id"] {
        let mut value = serde_json::to_value(&intent).map_err(io::Error::other)?;
        let payload = value[field]
            .as_object_mut()
            .unwrap()
            .values_mut()
            .next()
            .unwrap();
        payload["futureAuthority"] = serde_json::json!(true);
        assert!(serde_json::from_value::<RolloutMoveIntent>(value).is_err());
    }
    let mut value = serde_json::to_value(&intent).map_err(io::Error::other)?;
    value["futureAuthority"] = serde_json::json!(true);
    assert!(serde_json::from_value::<RolloutMoveIntent>(value).is_err());
    let mut publication = serde_json::json!({"path": path, "identity": identity});
    assert!(
        serde_json::from_value::<super::super::receipt_publication::ReceiptPublication>(
            publication.clone()
        )
        .is_ok()
    );
    publication["futureAuthority"] = serde_json::json!(true);
    assert!(
        serde_json::from_value::<super::super::receipt_publication::ReceiptPublication>(
            publication
        )
        .is_err()
    );
    let mut duplicate = original;
    duplicate.pop();
    duplicate.extend_from_slice(b",\"source_len\":5}");
    assert!(serde_json::from_slice::<RolloutMoveIntent>(&duplicate).is_err());
    assert_eq!(std::fs::read(&path)?, b"owned");
    Ok(())
}
