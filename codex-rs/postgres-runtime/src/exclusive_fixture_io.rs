//! Bounded private fixture inputs and owned command capture.
use std::io::Read;
use std::path::Path;
use std::process::Output;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::AsyncRead;
use tokio::io::AsyncReadExt;
use tokio::process::Command;

pub(super) fn read_file(path: &Path, limit: usize) -> Result<Vec<u8>, &'static str> {
    use std::os::unix::fs::OpenOptionsExt;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|_| "fixture_input_unavailable")?;
    if !file
        .metadata()
        .map_err(|_| "fixture_input_unavailable")?
        .is_file()
    {
        return Err("fixture_input_not_regular");
    }
    let mut bytes = Vec::with_capacity(limit + 1);
    file.take((limit + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| "fixture_input_unavailable")?;
    if bytes.len() > limit {
        return Err("fixture_input_oversized");
    }
    Ok(bytes)
}

async fn read_pipe(
    pipe: &mut (impl AsyncRead + Unpin),
    limit: usize,
) -> Result<Vec<u8>, &'static str> {
    let mut bytes = Vec::with_capacity(limit + 1);
    pipe.take((limit + 1) as u64)
        .read_to_end(&mut bytes)
        .await
        .map_err(|_| "identity_capture_failed")?;
    if bytes.len() > limit {
        return Err("identity_output_oversized");
    }
    Ok(bytes)
}

/// The owner must fully join this future; cancellation cannot certify child disposal.
/// Timeout and capture failure kill and reap the exact child before returning refusal.
pub(super) async fn capture(
    command: &mut Command,
    deadline: Duration,
    limit: usize,
) -> Result<Output, &'static str> {
    command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = command.spawn().map_err(|_| "identity_inspection_failed")?;
    let pipes = (child.stdout.take(), child.stderr.take());
    let (Some(mut stdout), Some(mut stderr)) = pipes else {
        let _ = child.start_kill();
        child.wait().await.map_err(|_| "identity_disposal_failed")?;
        return Err("identity_capture_failed");
    };
    let captured = tokio::time::timeout(deadline, async {
        tokio::try_join!(
            async { child.wait().await.map_err(|_| "identity_inspection_failed") },
            read_pipe(&mut stdout, limit),
            read_pipe(&mut stderr, limit),
        )
    })
    .await;
    drop(stdout);
    drop(stderr);
    match captured {
        Ok(Ok((status, stdout, stderr))) => Ok(Output {
            status,
            stdout,
            stderr,
        }),
        failure => {
            let primary = match failure {
                Err(_) => "identity_inspection_timeout",
                Ok(Err(code)) => code,
                Ok(Ok(_)) => unreachable!("successful capture handled above"),
            };
            let _ = child.start_kill();
            child.wait().await.map_err(|_| "identity_disposal_failed")?;
            Err(primary)
        }
    }
}

#[cfg(test)]
#[path = "exclusive_fixture_io_tests.rs"]
mod tests;
