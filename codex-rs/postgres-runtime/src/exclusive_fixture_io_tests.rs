use super::capture;
use super::read_file;
use pretty_assertions::assert_eq;
use std::time::Duration;
use tokio::process::Command;

#[test]
fn oversized_input_is_refused_and_unchanged() {
    let state = tempfile::tempdir().expect("private fixture directory");
    let path = state.path().join("receipt.json");
    let original = vec![b'x'; 65_537];
    std::fs::write(&path, &original).expect("write oversized receipt");
    assert_eq!(
        read_file(&path, /*limit*/ 65_536),
        Err("fixture_input_oversized")
    );
    assert_eq!(std::fs::read(path).expect("preserved receipt"), original);
}

#[cfg(unix)]
#[tokio::test]
async fn both_capture_streams_preserve_exact_bounded_output() {
    let mut command = Command::new("sh");
    command.args(["-c", "printf out; printf err >&2"]);
    let output = capture(
        &mut command,
        Duration::from_secs(/*secs*/ 2),
        /*limit*/ 3,
    )
    .await
    .expect("bounded capture");
    assert!(output.status.success());
    assert_eq!(
        (output.stdout, output.stderr),
        (b"out".to_vec(), b"err".to_vec())
    );
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn timeout_reaps_the_exact_owned_child() {
    let state = tempfile::tempdir().expect("private fixture directory");
    let pid_path = state.path().join("pid");
    let mut command = Command::new("sh");
    command
        .args(["-c", "printf '%s' $$ > \"$1\"; exec sleep 30", "fixture"])
        .arg(&pid_path);
    assert_eq!(
        capture(
            &mut command,
            Duration::from_secs(/*secs*/ 2),
            /*limit*/ 8
        )
        .await
        .err(),
        Some("identity_inspection_timeout")
    );
    let pid = std::fs::read_to_string(pid_path).expect("owned child reached exercise");
    assert!(pid.bytes().all(|byte| byte.is_ascii_digit()));
    assert!(!std::path::Path::new("/proc").join(pid).exists());
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn overflow_reaps_the_exact_owned_child_for_each_stream() {
    for script in [
        "printf '%s' $$ > \"$1\"; printf 123456789; exec sleep 30",
        "printf '%s' $$ > \"$1\"; printf 123456789 >&2; exec sleep 30",
    ] {
        let state = tempfile::tempdir().expect("private fixture directory");
        let pid_path = state.path().join("pid");
        let mut command = Command::new("sh");
        command.args(["-c", script, "fixture"]).arg(&pid_path);
        assert_eq!(
            capture(
                &mut command,
                Duration::from_secs(/*secs*/ 2),
                /*limit*/ 8
            )
            .await
            .err(),
            Some("identity_output_oversized")
        );
        let pid = std::fs::read_to_string(pid_path).expect("owned child reached exercise");
        assert!(pid.bytes().all(|byte| byte.is_ascii_digit()));
        assert!(!std::path::Path::new("/proc").join(pid).exists());
    }
}
#[test]
fn exact_credential_bound_is_accepted_and_next_byte_is_refused() {
    let state = tempfile::tempdir().expect("private fixture directory");
    let path = state.path().join("runtime.password");
    let exact = vec![b'p'; 4096];
    std::fs::write(&path, &exact).expect("write exact credential");
    assert_eq!(read_file(&path, /*limit*/ 4096), Ok(exact.clone()));
    let mut oversized = exact;
    oversized.push(b'q');
    std::fs::write(&path, &oversized).expect("write oversized credential");
    assert_eq!(
        read_file(&path, /*limit*/ 4096),
        Err("fixture_input_oversized")
    );
    assert_eq!(
        std::fs::read(path).expect("preserved credential"),
        oversized
    );
}
#[cfg(unix)]
#[test]
fn symlink_input_is_refused_without_touching_the_target() {
    let state = tempfile::tempdir().expect("private fixture directory");
    let target = state.path().join("target");
    let link = state.path().join("receipt.json");
    std::fs::write(&target, b"retained target").expect("write target");
    std::os::unix::fs::symlink(&target, &link).expect("create symlink");
    assert_eq!(
        read_file(&link, /*limit*/ 64),
        Err("fixture_input_unavailable")
    );
    assert_eq!(std::fs::read_link(link).expect("preserved link"), target);
    assert_eq!(
        std::fs::read(target).expect("preserved target"),
        b"retained target"
    );
}

#[cfg(unix)]
#[test]
fn fifo_input_is_refused_without_a_writer_and_preserved() {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::FileTypeExt;
    use std::os::unix::fs::MetadataExt;
    let state = tempfile::tempdir().expect("private fixture directory");
    let path = state.path().join("receipt.json");
    let native = std::ffi::CString::new(path.as_os_str().as_bytes()).expect("native path");
    // SAFETY: native is a live NUL-terminated path owned by this test.
    assert_eq!(
        unsafe {
            libc::mkfifo(native.as_ptr(), /*mode*/ 0o600)
        },
        0
    );
    let before = std::fs::symlink_metadata(&path).expect("fifo identity");
    assert_eq!(
        read_file(&path, /*limit*/ 64),
        Err("fixture_input_not_regular")
    );
    let after = std::fs::symlink_metadata(path).expect("preserved fifo");
    assert!(after.file_type().is_fifo());
    assert_eq!((after.dev(), after.ino()), (before.dev(), before.ino()));
}
