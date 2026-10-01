use std::path::PathBuf;

use pretty_assertions::assert_eq;
use serde::Deserialize;
use serde::Serialize;

#[derive(Debug, Deserialize, PartialEq, Serialize)]
struct PathRecord {
    #[serde(with = "super")]
    path: PathBuf,
}

#[test]
fn utf8_paths_keep_the_existing_json_string_format() -> serde_json::Result<()> {
    let original = PathRecord {
        path: PathBuf::from("sessions/2025/rollout.jsonl"),
    };
    let json = serde_json::to_string(&original)?;

    assert_eq!(json, r#"{"path":"sessions/2025/rollout.jsonl"}"#);
    assert_eq!(serde_json::from_str::<PathRecord>(&json)?, original);
    Ok(())
}

#[cfg(unix)]
#[test]
fn unix_paths_with_invalid_utf8_round_trip_without_loss() -> serde_json::Result<()> {
    use std::os::unix::ffi::OsStringExt;

    let original = PathRecord {
        path: std::ffi::OsString::from_vec(b"sessions/\xff/rollout.jsonl".to_vec()).into(),
    };
    let json = serde_json::to_string(&original)?;

    assert!(json.contains("unixHex"));
    assert_eq!(serde_json::from_str::<PathRecord>(&json)?, original);
    Ok(())
}

#[cfg(windows)]
#[test]
fn windows_paths_with_unpaired_surrogates_round_trip_without_loss() -> serde_json::Result<()> {
    use std::os::windows::ffi::OsStringExt;

    let original = PathRecord {
        path: std::ffi::OsString::from_wide(&[b'a' as u16, 0xd800, b'b' as u16]).into(),
    };
    let json = serde_json::to_string(&original)?;

    assert!(json.contains("windowsWideHex"));
    assert_eq!(serde_json::from_str::<PathRecord>(&json)?, original);
    Ok(())
}

#[test]
fn malformed_or_other_platform_native_paths_are_rejected() {
    #[cfg(unix)]
    let invalid = [
        r#"{"path":{"unixHex":"f"}}"#,
        r#"{"path":{"unixHex":"fg"}}"#,
        r#"{"path":{"unixHex":"00"}}"#,
        r#"{"path":{"unixHex":"ff","extra":true}}"#,
        r#"{"path":{"windowsWideHex":"d800"}}"#,
    ];
    #[cfg(windows)]
    let invalid = [
        r#"{"path":{"windowsWideHex":"f"}}"#,
        r#"{"path":{"windowsWideHex":"fg"}}"#,
        r#"{"path":{"windowsWideHex":"00"}}"#,
        r#"{"path":{"windowsWideHex":"0000"}}"#,
        r#"{"path":{"windowsWideHex":"d800","extra":true}}"#,
        r#"{"path":{"unixHex":"ff"}}"#,
    ];
    for record in invalid {
        assert!(serde_json::from_str::<PathRecord>(record).is_err());
    }
}
