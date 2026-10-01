use pretty_assertions::assert_eq;

use super::*;

fn prepare_move_intent(source: &Path, destination: &Path) -> io::Result<RolloutMoveIntent> {
    let parent = std::fs::canonicalize(
        destination
            .parent()
            .ok_or_else(|| io::Error::other("no parent"))?,
    )?;
    let mut stage = tempfile::Builder::new()
        .prefix(".codex-rollout-stage-")
        .tempfile_in(&parent)?;
    io::copy(&mut std::fs::File::open(source)?, &mut stage)?;
    stage.as_file().sync_all()?;
    let stage_id = rollout_file_identity(stage.path())?;
    let stage_digest = rollout_file_digest(stage.path())?;
    let (_file, stage_path) = stage.keep().map_err(|err| err.error)?;
    let source_metadata = std::fs::metadata(source)?;
    let intent = RolloutMoveIntent {
        source: std::fs::canonicalize(source)?,
        destination: parent.join(
            destination
                .file_name()
                .ok_or_else(|| io::Error::other("no file name"))?,
        ),
        source_len: source_metadata.len(),
        source_modified: source_metadata.modified()?,
        source_id: rollout_file_identity(source)?,
        stage_path,
        stage_id,
        stage_digest,
    };
    write_rollout_move_intent(
        &rollout_move_intent_path(destination),
        &intent,
        sync_parent_directory,
    )?;
    Ok(intent)
}

#[test]
fn retry_preserves_stage_after_published_intent_sync_fails() -> io::Result<()> {
    let home = tempfile::tempdir()?;
    let sessions = home.path().join(codex_rollout::SESSIONS_SUBDIR);
    let archived = home.path().join(codex_rollout::ARCHIVED_SESSIONS_SUBDIR);
    std::fs::create_dir(&sessions)?;
    std::fs::create_dir(&archived)?;
    let source = sessions.join("rollout.jsonl");
    let destination = archived.join("rollout.jsonl");
    std::fs::write(&source, b"rollout contents")?;

    let error = move_rollout_noclobber_retained_with_intent_sync(
        &source,
        &destination,
        home.path(),
        |path| {
            assert!(path.exists(), "the intent must already be published");
            Err(io::Error::other("injected intent directory sync failure"))
        },
    )
    .expect_err("failed intent sync must be reported");
    assert_eq!(error.to_string(), "injected intent directory sync failure");
    let intent = read_rollout_move_intent(&rollout_move_intent_path(&destination))?;
    assert_eq!(std::fs::read(&source)?, b"rollout contents");
    assert_eq!(std::fs::read(&intent.stage_path)?, b"rollout contents");
    assert!(!destination.exists());

    move_rollout_noclobber(&source, &destination, home.path())?;
    assert!(!source.exists());
    assert!(!intent.stage_path.exists());
    assert!(!rollout_move_intent_path(&destination).exists());
    assert_eq!(std::fs::read(&destination)?, b"rollout contents");
    Ok(())
}

#[test]
fn oversized_intent_preserves_source_before_publication() -> io::Result<()> {
    let root = tempfile::tempdir()?;
    let mut home = root.path().to_path_buf();
    for _ in 0..10 {
        home.push("p".repeat(/*n*/ 140));
    }
    let sessions = home.join(codex_rollout::SESSIONS_SUBDIR);
    let archived = home.join(codex_rollout::ARCHIVED_SESSIONS_SUBDIR);
    std::fs::create_dir_all(&sessions)?;
    std::fs::create_dir_all(&archived)?;
    let source = sessions.join("rollout.jsonl");
    let destination = archived.join("rollout.jsonl");
    std::fs::write(&source, b"rollout contents")?;

    let error = move_rollout_noclobber_retained(&source, &destination, &home)
        .expect_err("oversized receipt must fail before source unlink");
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert_eq!(std::fs::read(&source)?, b"rollout contents");
    assert!(!destination.exists());
    assert!(!rollout_move_intent_path(&destination).exists());
    Ok(())
}

#[test]
fn retry_completes_a_published_staged_move() -> std::io::Result<()> {
    let home = tempfile::tempdir()?;
    let sessions = home.path().join(codex_rollout::SESSIONS_SUBDIR);
    let archived = home.path().join(codex_rollout::ARCHIVED_SESSIONS_SUBDIR);
    std::fs::create_dir(&sessions)?;
    std::fs::create_dir(&archived)?;
    let source = sessions.join("rollout.jsonl");
    let destination = archived.join("rollout.jsonl");
    std::fs::write(&source, b"rollout contents")?;

    // Fault point: the process exits after publication and before source unlink.
    let intent = prepare_move_intent(&source, &destination)?;
    tempfile::TempPath::try_from_path(intent.stage_path)?.persist_noclobber(&destination)?;
    move_rollout_noclobber(&source, &destination, home.path())?;

    assert!(!source.exists());
    assert_eq!(std::fs::read(&destination)?, b"rollout contents");
    assert!(!rollout_move_intent_path(&destination).exists());
    Ok(())
}

#[test]
fn retry_publishes_a_prepared_stage_after_crash() -> std::io::Result<()> {
    let home = tempfile::tempdir()?;
    let sessions = home.path().join(codex_rollout::SESSIONS_SUBDIR);
    let archived = home.path().join(codex_rollout::ARCHIVED_SESSIONS_SUBDIR);
    std::fs::create_dir(&sessions)?;
    std::fs::create_dir(&archived)?;
    let source = sessions.join("rollout.jsonl");
    let destination = archived.join("rollout.jsonl");
    std::fs::write(&source, b"rollout contents")?;
    let intent = prepare_move_intent(&source, &destination)?;

    move_rollout_noclobber(&source, &destination, home.path())?;

    assert!(!source.exists());
    assert_eq!(std::fs::read(&destination)?, b"rollout contents");
    assert!(!intent.stage_path.exists());
    assert!(!rollout_move_intent_path(&destination).exists());
    Ok(())
}

#[test]
fn retry_preserves_source_if_published_bytes_change_in_place() -> std::io::Result<()> {
    let home = tempfile::tempdir()?;
    let sessions = home.path().join(codex_rollout::SESSIONS_SUBDIR);
    let archived = home.path().join(codex_rollout::ARCHIVED_SESSIONS_SUBDIR);
    std::fs::create_dir(&sessions)?;
    std::fs::create_dir(&archived)?;
    let source = sessions.join("rollout.jsonl");
    let destination = archived.join("rollout.jsonl");
    std::fs::write(&source, b"rollout contents")?;
    let intent = prepare_move_intent(&source, &destination)?;
    tempfile::TempPath::try_from_path(intent.stage_path)?.persist_noclobber(&destination)?;
    let published_id = rollout_file_identity(&destination)?;
    std::fs::write(&destination, b"changed in place")?;

    let err = move_rollout_noclobber(&source, &destination, home.path())
        .expect_err("modified published bytes must not consume source");

    assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
    assert_eq!(rollout_file_identity(&destination)?, published_id);
    assert_eq!(std::fs::read(&source)?, b"rollout contents");
    Ok(())
}

#[test]
fn retry_rejects_competing_hard_link_to_source() -> std::io::Result<()> {
    let home = tempfile::tempdir()?;
    let sessions = home.path().join(codex_rollout::SESSIONS_SUBDIR);
    let archived = home.path().join(codex_rollout::ARCHIVED_SESSIONS_SUBDIR);
    std::fs::create_dir(&sessions)?;
    std::fs::create_dir(&archived)?;
    let source = sessions.join("rollout.jsonl");
    let destination = archived.join("rollout.jsonl");
    std::fs::write(&source, b"rollout contents")?;
    let intent = prepare_move_intent(&source, &destination)?;
    std::fs::hard_link(&source, &destination)?;

    let error = move_rollout_noclobber(&source, &destination, home.path())
        .expect_err("a hard link to the source is not the staged file");

    assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
    assert!(source.exists());
    assert!(destination.exists());
    assert!(intent.stage_path.exists());
    assert!(rollout_move_intent_path(&destination).exists());
    Ok(())
}

#[test]
fn retry_preserves_an_unrelated_destination() -> std::io::Result<()> {
    let home = tempfile::tempdir()?;
    let sessions = home.path().join(codex_rollout::SESSIONS_SUBDIR);
    let archived = home.path().join(codex_rollout::ARCHIVED_SESSIONS_SUBDIR);
    std::fs::create_dir(&sessions)?;
    std::fs::create_dir(&archived)?;
    let source = sessions.join("rollout.jsonl");
    let destination = archived.join("rollout.jsonl");
    std::fs::write(&source, b"rollout contents")?;
    std::fs::hard_link(&source, &destination)?;

    let error = move_rollout_noclobber(&source, &destination, home.path())
        .expect_err("unrelated destination must not be removed");

    assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
    assert_eq!(std::fs::read(&source)?, b"rollout contents");
    assert_eq!(std::fs::read(&destination)?, b"rollout contents");
    Ok(())
}

#[test]
fn retry_preserves_ambiguous_copy_after_intent() -> std::io::Result<()> {
    let home = tempfile::tempdir()?;
    let sessions = home.path().join(codex_rollout::SESSIONS_SUBDIR);
    let archived = home.path().join(codex_rollout::ARCHIVED_SESSIONS_SUBDIR);
    std::fs::create_dir(&sessions)?;
    std::fs::create_dir(&archived)?;
    let source = sessions.join("rollout.jsonl");
    let destination = archived.join("rollout.jsonl");
    std::fs::write(&source, b"rollout contents")?;
    let intent = prepare_move_intent(&source, &destination)?;
    std::fs::write(&destination, b"partial copy")?;

    let error = move_rollout_noclobber(&source, &destination, home.path())
        .expect_err("a copy with a different identity cannot be reconciled");

    assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
    assert_eq!(std::fs::read(&source)?, b"rollout contents");
    assert_eq!(std::fs::read(&destination)?, b"partial copy");
    assert!(rollout_move_intent_path(&destination).exists());
    assert!(intent.stage_path.exists());
    Ok(())
}

#[test]
fn published_receipt_verifies_destination_after_source_unlink() -> std::io::Result<()> {
    let home = tempfile::tempdir()?;
    let sessions = home.path().join(codex_rollout::SESSIONS_SUBDIR);
    let archived = home.path().join(codex_rollout::ARCHIVED_SESSIONS_SUBDIR);
    std::fs::create_dir(&sessions)?;
    std::fs::create_dir(&archived)?;
    let source = sessions.join("rollout.jsonl");
    let destination = archived.join("rollout.jsonl");
    std::fs::write(&source, b"rollout contents")?;

    move_rollout_noclobber_retained(&source, &destination, home.path())?;
    verify_published_rollout_move(&source, &destination, home.path())?;

    std::fs::rename(&destination, archived.join("original.jsonl"))?;
    std::fs::write(&destination, b"replacement")?;
    assert!(verify_published_rollout_move(&source, &destination, home.path()).is_err());
    Ok(())
}
