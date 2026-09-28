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
