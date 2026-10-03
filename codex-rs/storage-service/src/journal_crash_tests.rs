use super::*;
use std::process::Command;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;
use std::time::Instant;

fn record(state: OperationState) -> OperationRecord {
    OperationRecord {
        operation_id: Uuid::from_u128(99),
        action: PlanAction::Return,
        plan_digest: "exact-owner".into(),
        state,
        run_id: Some(Uuid::from_u128(100)),
        return_source: Some(ReturnSource {
            dataset_id: Uuid::from_u128(101),
            generation: 2,
            destination: "fixture".into(),
        }),
        created_at_ms: 1,
        updated_at_ms: 2,
        blocker: None,
        copied: vec![("threads".into(), 7)],
    }
}

#[test]
fn journal_crash_child() {
    let Some(home) = std::env::var_os("CODEX_JOURNAL_CRASH_HOME") else {
        return;
    };
    let mode = std::env::var("CODEX_JOURNAL_CRASH_MODE").expect("mode");
    let phase = match std::env::var("CODEX_JOURNAL_CRASH_PHASE")
        .expect("phase")
        .as_str()
    {
        "record-file" => "record-file",
        "update-file" => "update-file",
        "journal-parent" => "journal-parent",
        _ => panic!("unknown phase"),
    };
    let after = std::env::var("CODEX_JOURNAL_CRASH_AFTER").expect("after") == "true";
    let home = PathBuf::from(home);
    let observer = home.join("observer-success");
    let mut journal = Journal::new(&home).observed_by(Arc::new(move |_| {
        std::fs::write(&observer, b"unexpected success").expect("observer marker");
    }));
    journal.sync_probe = Some(Arc::new(Mutex::new(SyncProbe {
        crash: Some((phase, after, home.join("crash-witness"))),
        ..Default::default()
    })));
    if mode == "create" {
        journal
            .create(&record(OperationState::Planned))
            .expect("create");
    } else {
        assert_eq!(mode, "update");
        journal
            .update(&record(OperationState::Ready))
            .expect("update");
    }
    panic!("crash boundary was not reached");
}

#[test]
fn subprocess_crash_preserves_exact_single_owner_without_success_observer() {
    let test_name = format!(
        "{}::journal_crash_child",
        module_path!().split_once("::").expect("module").1
    );
    for (mode, phase) in [
        ("create", "record-file"),
        ("create", "journal-parent"),
        ("update", "update-file"),
        ("update", "journal-parent"),
    ] {
        for after in [false, true] {
            let home = tempfile::tempdir().expect("home");
            let planned = record(OperationState::Planned);
            let ready = record(OperationState::Ready);
            if mode == "update" {
                Journal::new(home.path())
                    .create(&planned)
                    .expect("baseline");
            }
            let mut child = Command::new(std::env::current_exe().expect("test executable"))
                .args(["--exact", &test_name, "--test-threads=1"])
                .env_remove("CODEX_JOURNAL_CRASH_HOME")
                .env("CODEX_JOURNAL_CRASH_HOME", home.path())
                .env("CODEX_JOURNAL_CRASH_MODE", mode)
                .env("CODEX_JOURNAL_CRASH_PHASE", phase)
                .env("CODEX_JOURNAL_CRASH_AFTER", after.to_string())
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::inherit())
                .spawn()
                .expect("owned child");
            let deadline = Instant::now() + Duration::from_secs(15);
            let observed = loop {
                match child.try_wait() {
                    Ok(Some(status)) => break Ok(status),
                    Ok(None) if Instant::now() < deadline => {
                        std::thread::sleep(Duration::from_millis(10))
                    }
                    Ok(None) => break Err("crash child timeout".to_owned()),
                    Err(error) => break Err(error.to_string()),
                }
            };
            // Reap this exact captured subprocess even on a timeout/readback failure.
            if observed.is_err() {
                let _ = child.kill();
            }
            let terminal = loop {
                match child.wait() {
                    Ok(status) => break status,
                    Err(error) => {
                        eprintln!("exact crash child retained: terminal wait uncertain: {error}");
                        std::thread::sleep(Duration::from_millis(100));
                    }
                }
            };
            let status = observed.expect("bounded child terminal");
            assert_eq!(terminal, status);
            assert_eq!(status.code(), Some(87));
            assert_eq!(
                std::fs::read_to_string(home.path().join("crash-witness"))
                    .expect("actual seam witness"),
                format!("{phase}:{after}")
            );
            assert!(!home.path().join("observer-success").exists());
            let fresh = Journal::new(home.path());
            let expected = if mode == "update" && phase == "update-file" {
                &planned
            } else if mode == "update" {
                &ready
            } else {
                &planned
            };
            assert_eq!(
                fresh
                    .read(planned.operation_id)
                    .expect("fresh read")
                    .as_ref(),
                Some(expected)
            );
            assert_eq!(
                fresh.list_checked().expect("single exact owner"),
                vec![expected.clone()]
            );
            assert_eq!(
                std::fs::read_dir(&fresh.directory)
                    .unwrap()
                    .any(|entry| entry
                        .unwrap()
                        .path()
                        .extension()
                        .is_some_and(|value| value == "tmp")),
                mode == "update" && phase == "update-file"
            );
            assert!(
                fresh.create(&planned).is_err(),
                "create cannot mint a second owner"
            );
            fresh.update(&ready).expect("same-owner recovery update");
            assert_eq!(
                Journal::new(home.path())
                    .list_checked()
                    .expect("fresh recovered"),
                vec![ready]
            );
        }
    }
}
