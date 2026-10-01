use pretty_assertions::assert_eq;

use super::rollout_file_identity;

#[test]
fn identity_matches_hard_links_and_rejects_distinct_files() -> std::io::Result<()> {
    let home = tempfile::tempdir()?;
    let source = home.path().join("source.jsonl");
    let linked = home.path().join("linked.jsonl");
    let distinct = home.path().join("distinct.jsonl");
    std::fs::write(&source, b"same bytes")?;
    std::fs::hard_link(&source, &linked)?;
    std::fs::write(&distinct, b"same bytes")?;

    assert_eq!(
        rollout_file_identity(&source)?,
        rollout_file_identity(&linked)?
    );
    assert_ne!(
        rollout_file_identity(&source)?,
        rollout_file_identity(&distinct)?
    );
    Ok(())
}

#[test]
fn digest_tracks_actual_in_place_content_while_identity_stays_owned() -> std::io::Result<()> {
    let home = tempfile::tempdir()?;
    let source = home.path().join("source.jsonl");
    let linked = home.path().join("linked.jsonl");
    std::fs::write(&source, b"first payload")?;
    std::fs::hard_link(&source, &linked)?;
    let identity = super::rollout_file_identity(&source)?;
    let before = super::rollout_file_digest(&source)?;
    assert_eq!(super::rollout_file_digest(&linked)?, before);
    std::fs::write(&source, b"second payload")?;
    let after = super::rollout_file_digest(&source)?;
    assert_ne!(after, before);
    assert_eq!(super::rollout_file_digest(&linked)?, after);
    assert_eq!(
        (
            super::rollout_file_identity(&source)?,
            super::rollout_file_identity(&linked)?
        ),
        (identity, identity)
    );
    assert_eq!(std::fs::read(&linked)?, b"second payload");
    assert_eq!(
        super::rollout_file_digest(home.path())
            .err()
            .ok_or_else(|| std::io::Error::other("directory accepted"))?
            .to_string(),
        "rollout move path is not a regular file"
    );
    Ok(())
}

#[test]
fn identity_and_digest_refuse_directory_replacement_after_path_check() -> std::io::Result<()> {
    #[derive(Clone, Copy)]
    enum Reader {
        Identity,
        Digest,
    }
    for reader in [Reader::Identity, Reader::Digest] {
        let home = tempfile::tempdir()?;
        let source = home.path().join("source");
        let retained = home.path().join("retained");
        std::fs::write(&source, b"owned original")?;
        let identity = super::rollout_file_identity(&source)?;
        let hit = std::cell::Cell::new(0);
        let replace = || {
            hit.set(hit.get() + 1);
            std::fs::rename(&source, &retained)?;
            std::fs::create_dir(&source)
        };
        let result = match reader {
            Reader::Identity => super::identity_before_open(&source, replace).map(|_| ()),
            Reader::Digest => super::digest_before_open(&source, replace).map(|_| ()),
        };
        assert!(result.is_err());
        assert_eq!(hit.get(), 1);
        assert!(source.is_dir());
        assert_eq!(
            (
                std::fs::read(&retained)?,
                super::rollout_file_identity(&retained)?
            ),
            (b"owned original".to_vec(), identity)
        );
        assert_eq!(std::fs::read_dir(&source)?.count(), 0);
    }
    Ok(())
}

#[cfg(unix)]
#[test]
fn identity_and_digest_refuse_symlink_swap_without_reading_foreign_bytes() -> std::io::Result<()> {
    #[derive(Clone, Copy)]
    enum Reader {
        Identity,
        Digest,
    }
    for reader in [Reader::Identity, Reader::Digest] {
        let home = tempfile::tempdir()?;
        let source = home.path().join("source");
        let retained = home.path().join("retained");
        let foreign = home.path().join("foreign");
        std::fs::write(&source, b"owned original")?;
        std::fs::write(&foreign, b"foreign content")?;
        let original_id = super::rollout_file_identity(&source)?;
        let foreign_id = super::rollout_file_identity(&foreign)?;
        let hit = std::cell::Cell::new(0);
        let replace = || {
            hit.set(hit.get() + 1);
            std::fs::rename(&source, &retained)?;
            std::os::unix::fs::symlink(&foreign, &source)
        };
        let result = match reader {
            Reader::Identity => super::identity_before_open(&source, replace).map(|_| ()),
            Reader::Digest => super::digest_before_open(&source, replace).map(|_| ()),
        };
        assert!(result.is_err());
        assert_eq!(hit.get(), 1);
        assert!(std::fs::symlink_metadata(&source)?.file_type().is_symlink());
        assert_eq!(
            (
                std::fs::read(&retained)?,
                super::rollout_file_identity(&retained)?
            ),
            (b"owned original".to_vec(), original_id)
        );
        assert_eq!(
            (
                std::fs::read(&foreign)?,
                super::rollout_file_identity(&foreign)?
            ),
            (b"foreign content".to_vec(), foreign_id)
        );
    }
    Ok(())
}

#[test]
fn hashing_refuses_growth_same_length_rewrite_and_namespace_replacement_after_snapshot()
-> std::io::Result<()> {
    enum Case {
        Growth,
        SameLength,
        Namespace,
    }
    for case in [Case::Growth, Case::SameLength, Case::Namespace] {
        let home = tempfile::tempdir()?;
        let path = home.path().join("rollout");
        let retained = home.path().join("retained");
        let original = b"original payload";
        std::fs::write(&path, original)?;
        let original_id = super::rollout_file_identity(&path)?;
        let before = std::fs::metadata(&path)?;
        let hit = std::cell::Cell::new(0);
        let read_hit = std::cell::Cell::new(0);
        let replacement = match case {
            Case::Growth => b"original payload plus substantial appended bytes".as_slice(),
            Case::SameLength => b"modified payload".as_slice(),
            Case::Namespace => original.as_slice(),
        };
        let error = super::digest_at_read_boundaries(
            &path,
            || Ok(()),
            || {
                hit.set(hit.get() + 1);
                match case {
                    Case::Growth => std::fs::write(&path, replacement)?,
                    Case::SameLength => {
                        std::fs::write(&path, replacement)?;
                        std::fs::File::options()
                            .write(true)
                            .open(&path)?
                            .set_times(std::fs::FileTimes::new().set_modified(
                                before.modified()? + std::time::Duration::from_secs(2),
                            ))?;
                    }
                    Case::Namespace => {
                        std::fs::rename(&path, &retained)?;
                        std::fs::write(&path, replacement)?;
                    }
                }
                Ok(())
            },
            |consumed| {
                read_hit.set(read_hit.get() + 1);
                assert_eq!(
                    consumed,
                    match case {
                        Case::Growth => original.len() as u64 + 1,
                        Case::SameLength | Case::Namespace => original.len() as u64,
                    }
                );
                Ok(())
            },
        )
        .unwrap_err();
        assert_eq!(error.to_string(), "rollout changed while hashing");
        assert_eq!((hit.get(), read_hit.get()), (1, 1));
        assert_eq!(std::fs::read(&path)?, replacement);
        match case {
            Case::Namespace => {
                assert_eq!(
                    (
                        std::fs::read(&retained)?,
                        super::rollout_file_identity(&retained)?
                    ),
                    (original.to_vec(), original_id)
                );
                assert_ne!(super::rollout_file_identity(&path)?, original_id);
            }
            Case::Growth | Case::SameLength => {
                assert_eq!(super::rollout_file_identity(&path)?, original_id)
            }
        }
        assert_eq!(
            super::rollout_file_digest(&path)?,
            *blake3::hash(replacement).as_bytes()
        );
    }
    Ok(())
}
