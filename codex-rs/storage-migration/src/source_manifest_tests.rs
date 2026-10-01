use super::*;
use pretty_assertions::assert_eq;
use sha2::Digest;
use sha2::Sha256;

fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[test]
fn the_manifest_names_every_database_and_history_directory_and_is_kept_once() {
    let home = tempfile::tempdir().expect("home");
    std::fs::write(home.path().join("state_5.sqlite"), b"state").expect("state");
    std::fs::write(home.path().join("state_5.sqlite-wal"), b"log").expect("wal");
    std::fs::write(home.path().join("config.toml"), b"not history").expect("config");
    std::fs::create_dir_all(home.path().join("sessions/2025")).expect("sessions");
    std::fs::write(home.path().join("sessions/2025/a.jsonl"), b"12345").expect("rollout");
    let run_id = Uuid::from_u128(9);

    let first = write_source_manifest(home.path(), run_id).expect("manifest");

    assert_eq!(
        first,
        SourceManifest {
            run_id,
            files: vec![
                SourceFile {
                    name: "state_5.sqlite".to_string(),
                    bytes: 5,
                    sha256: digest(b"state"),
                },
                SourceFile {
                    name: "state_5.sqlite-wal".to_string(),
                    bytes: 3,
                    sha256: digest(b"log"),
                },
            ],
            directories: vec![SourceDirectory {
                name: "sessions".to_string(),
                files: 1,
                bytes: 5,
            }],
        }
    );

    // A later activation attempt reports the first record even though the files moved on.
    std::fs::write(home.path().join("state_5.sqlite"), b"changed").expect("change");
    assert_eq!(
        write_source_manifest(home.path(), run_id).expect("again"),
        first
    );
}
