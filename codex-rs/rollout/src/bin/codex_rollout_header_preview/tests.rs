use super::MAX_COMPRESSED_BYTES;
use super::MAX_DECODED_BYTES;
use super::MAX_REQUEST_LINE;
use super::MAX_REQUESTS;
use super::run;
use pretty_assertions::assert_eq;
use std::fs;
use std::io::Cursor;
use std::io::Write;
use std::path::Path;
use tempfile::TempDir;
use uuid::Uuid;
#[test]
fn reads_physical_compressed_header_even_with_plain_sibling() -> anyhow::Result<()> {
    let home = TempDir::new()?;
    let thread_id = Uuid::from_u128(1);
    let ancestor = Uuid::from_u128(2);
    let relative = format!("sessions/2025/01/03/rollout-2025-01-03T12-00-00-{thread_id}.jsonl.zst");
    let compressed = home.path().join(&relative);
    fs::create_dir_all(compressed.parent().expect("parent"))?;
    let metadata = session_meta(thread_id, Some(ancestor));
    fs::write(
        &compressed,
        zstd::stream::encode_all(metadata.as_bytes(), 1)?,
    )?;
    fs::write(
        compressed.with_extension("jsonl"),
        "different plain contents",
    )?;
    let result = request(home.path(), &[relative])?;
    assert_eq!(
        result,
        vec![serde_json::json!({
            "status": "ok",
            "thread_id": thread_id.to_string(),
            "ancestor_rollout_id": ancestor.to_string(),
        })]
    );
    Ok(())
}
#[test]
fn header_prefix_can_be_verified_without_reading_large_tail() -> anyhow::Result<()> {
    let home = TempDir::new()?;
    let thread_id = Uuid::from_u128(8);
    let relative = format!("archived_sessions/rollout-2025-01-03T12-00-00-{thread_id}.jsonl.zst");
    let compressed = home.path().join(&relative);
    fs::create_dir_all(compressed.parent().expect("parent"))?;
    fs::write(
        &compressed,
        zstd::stream::encode_all(session_meta(thread_id, None).as_bytes(), 1)?,
    )?;
    fs::OpenOptions::new()
        .append(true)
        .open(&compressed)?
        .write_all(&vec![0xff; 1024 * 1024 + 1])?;
    assert_eq!(
        request(home.path(), &[relative])?,
        vec![serde_json::json!({
            "status": "ok",
            "thread_id": thread_id.to_string(),
            "ancestor_rollout_id": null,
        })]
    );
    Ok(())
}
#[test]
fn invalid_late_request_emits_no_per_file_results() -> anyhow::Result<()> {
    let home = TempDir::new()?;
    let thread_id = Uuid::from_u128(3);
    let relative = format!("archived_sessions/rollout-2025-01-03T12-00-00-{thread_id}.jsonl.zst");
    let compressed = home.path().join(&relative);
    fs::create_dir_all(compressed.parent().expect("parent"))?;
    fs::write(
        compressed,
        zstd::stream::encode_all(session_meta(thread_id, None).as_bytes(), 1)?,
    )?;
    let input = format!(
        "{}\n{}\n",
        serde_json::json!({"relative_path": relative}),
        serde_json::json!({"relative_path": "sessions/../secret"})
    );
    let mut output = Vec::new();
    assert_eq!(
        run(home.path(), Cursor::new(input), &mut output),
        Err("invalid_request")
    );
    assert!(output.is_empty());
    Ok(())
}
#[test]
fn malformed_and_oversized_headers_remain_unresolved() -> anyhow::Result<()> {
    let home = TempDir::new()?;
    let invalid_id = Uuid::from_u128(4);
    let oversized_id = Uuid::from_u128(5);
    let invalid = format!("archived_sessions/rollout-2025-01-03T12-00-00-{invalid_id}.jsonl.zst");
    let oversized =
        format!("archived_sessions/rollout-2025-01-03T12-00-00-{oversized_id}.jsonl.zst");
    let archive = home.path().join("archived_sessions");
    fs::create_dir_all(&archive)?;
    fs::write(home.path().join(&invalid), b"invalid zstd")?;
    let large_line = format!("{}\n", " ".repeat(MAX_DECODED_BYTES as usize + 1));
    fs::write(
        home.path().join(&oversized),
        zstd::stream::encode_all(large_line.as_bytes(), 1)?,
    )?;
    assert_eq!(
        request(home.path(), &[invalid, oversized])?,
        vec![
            serde_json::json!({"status": "unresolved", "code": "decode_error"}),
            serde_json::json!({"status": "unresolved", "code": "decoded_limit"}),
        ]
    );
    Ok(())
}
#[test]
fn oversized_decoder_window_is_unresolved() -> anyhow::Result<()> {
    let home = TempDir::new()?;
    let thread_id = Uuid::from_u128(9);
    let relative = format!("archived_sessions/rollout-2025-01-03T12-00-00-{thread_id}.jsonl.zst");
    let compressed = home.path().join(&relative);
    fs::create_dir_all(compressed.parent().expect("parent"))?;
    let mut encoder = zstd::stream::write::Encoder::new(Vec::new(), 1)?;
    encoder.window_log(24)?;
    encoder.write_all(session_meta(thread_id, None).as_bytes())?;
    encoder.write_all(&vec![b'a'; 16 * 1024 * 1024])?;
    fs::write(compressed, encoder.finish()?)?;
    assert_eq!(
        request(home.path(), &[relative])?,
        vec![serde_json::json!({"status": "unresolved", "code": "decode_error"})]
    );
    Ok(())
}
#[test]
fn compressed_prefix_exhaustion_is_unresolved() -> anyhow::Result<()> {
    let home = TempDir::new()?;
    let thread_id = Uuid::from_u128(10);
    let relative = format!("archived_sessions/rollout-2025-01-03T12-00-00-{thread_id}.jsonl.zst");
    let compressed = home.path().join(&relative);
    fs::create_dir_all(compressed.parent().expect("parent"))?;
    // A valid zstd skippable frame consumes the whole compressed prefix before any header.
    let mut frame = vec![0x50, 0x2a, 0x4d, 0x18];
    frame.extend((MAX_COMPRESSED_BYTES as u32).to_le_bytes());
    frame.resize(frame.len() + MAX_COMPRESSED_BYTES as usize, 0);
    fs::write(compressed, frame)?;
    assert_eq!(
        request(home.path(), &[relative])?,
        vec![serde_json::json!({"status": "unresolved", "code": "compressed_limit"})]
    );
    Ok(())
}

#[test]
fn request_and_line_bounds_are_rejected_before_output() -> anyhow::Result<()> {
    let home = TempDir::new()?;
    let path = format!(
        "archived_sessions/rollout-2025-01-03T12-00-00-{}.jsonl.zst",
        Uuid::from_u128(6)
    );
    let line = format!("{}\n", serde_json::json!({"relative_path": path}));
    let mut output = Vec::new();
    assert_eq!(
        run(
            home.path(),
            Cursor::new(line.repeat(MAX_REQUESTS + 1)),
            &mut output
        ),
        Err("request_limit")
    );
    assert!(output.is_empty());
    assert_eq!(
        run(home.path(), Cursor::new(format!("{line}\n")), &mut output),
        Err("invalid_request")
    );
    assert!(output.is_empty());
    assert_eq!(
        run(
            home.path(),
            Cursor::new(format!("{}\n", "x".repeat(MAX_REQUEST_LINE + 1))),
            &mut output
        ),
        Err("request_limit")
    );
    assert!(output.is_empty());
    Ok(())
}

#[cfg(unix)]
#[test]
fn symlinked_component_is_unresolved_without_reading_target() -> anyhow::Result<()> {
    use std::os::unix::fs::symlink;

    let home = TempDir::new()?;
    let outside = TempDir::new()?;
    let thread_id = Uuid::from_u128(7);
    let relative = format!("sessions/2025/01/03/rollout-2025-01-03T12-00-00-{thread_id}.jsonl.zst");
    fs::create_dir_all(home.path().join("sessions/2025/01"))?;
    symlink(outside.path(), home.path().join("sessions/2025/01/03"))?;
    fs::write(
        outside
            .path()
            .join(format!("rollout-2025-01-03T12-00-00-{thread_id}.jsonl.zst")),
        zstd::stream::encode_all(session_meta(thread_id, None).as_bytes(), 1)?,
    )?;

    assert_eq!(
        request(home.path(), &[relative])?,
        vec![serde_json::json!({"status": "unresolved", "code": "symlink"})]
    );
    Ok(())
}

fn request(root: &Path, paths: &[String]) -> anyhow::Result<Vec<serde_json::Value>> {
    let input = paths
        .iter()
        .map(|path| format!("{}\n", serde_json::json!({"relative_path": path})))
        .collect::<String>();
    let mut output = Vec::new();
    run(root, Cursor::new(input), &mut output).map_err(anyhow::Error::msg)?;
    output
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(serde_json::from_slice)
        .collect::<Result<Vec<_>, _>>()
        .map_err(Into::into)
}

fn session_meta(thread_id: Uuid, ancestor: Option<Uuid>) -> String {
    let history_base = ancestor.map(|thread_id| {
        serde_json::json!({
            "thread_id": thread_id,
            "end_ordinal_exclusive": 1,
            "end_byte_offset": 100,
        })
    });
    format!(
        "{}\n",
        serde_json::json!({
            "timestamp": "2025-01-03T12:00:00Z",
            "type": "session_meta",
            "payload": {
                "id": thread_id,
                "timestamp": "2025-01-03T12:00:00Z",
                "cwd": "/tmp",
                "originator": "test",
                "cli_version": "test",
                "source": "cli",
                "model_provider": "test-provider",
                "history_mode": "paginated",
                "history_base": history_base,
            },
        })
    )
}
