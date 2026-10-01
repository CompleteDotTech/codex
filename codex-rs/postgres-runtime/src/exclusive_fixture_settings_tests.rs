use super::settings;
use pretty_assertions::assert_eq;

#[tokio::test]
async fn invalid_receipt_is_refused_with_a_sanitized_error() {
    let state = tempfile::tempdir().expect("private fixture directory");
    std::fs::write(
        state.path().join("receipt.json"),
        b"secret malformed receipt",
    )
    .expect("write malformed receipt");
    assert_eq!(
        settings(state.path(), "runtime").await.err(),
        Some("fixture_receipt_invalid")
    );
    assert_eq!(
        std::fs::read(state.path().join("receipt.json")).expect("preserved receipt"),
        b"secret malformed receipt"
    );
}

#[tokio::test]
async fn mismatched_exclusive_identity_is_refused_before_inspection() {
    let state = tempfile::tempdir().expect("private fixture directory");
    let receipt = br#"{"instance":"0123456789abcdef0123456789abcdef"}"#;
    let exclusive = br#"{"instance":"fedcba9876543210fedcba9876543210","purpose":"postgres-runtime-fixture-exclusive","disposable":true}"#;
    std::fs::write(state.path().join("receipt.json"), receipt).expect("write receipt");
    std::fs::write(
        state.path().join("postgres-fixture-exclusive.json"),
        exclusive,
    )
    .expect("write exclusive receipt");
    assert_eq!(
        settings(state.path(), "runtime").await.err(),
        Some("exclusive_receipt_mismatch")
    );
    assert_eq!(
        std::fs::read(state.path().join("postgres-fixture-exclusive.json"))
            .expect("preserved exclusive receipt"),
        exclusive
    );
}
#[tokio::test]
async fn complete_attestation_constructs_settings_and_refuses_causal_mismatches() {
    use super::settings_with_inspection;
    use serde_json::json;
    use std::os::unix::process::ExitStatusExt;
    use std::sync::Arc;
    use std::sync::atomic::AtomicUsize;
    use std::sync::atomic::Ordering;
    use std::time::Duration;
    let state = tempfile::tempdir().expect("private fixture directory");
    let instance = "0123456789abcdef0123456789abcdef";
    let container = "a".repeat(64);
    let receipt = serde_json::to_vec(&json!({"instance":instance,"project":"codex-pg-fixture","engine_id":"engine","port":15432})).expect("receipt");
    let exclusive = serde_json::to_vec(&json!({"instance":instance,"purpose":"postgres-runtime-fixture-exclusive","disposable":true,"docker_context":"fixture","container_id":container})).expect("exclusive");
    std::fs::create_dir(state.path().join("secrets")).expect("credential directory");
    let inputs = [
        ("receipt.json", receipt),
        ("postgres-fixture-exclusive.json", exclusive),
        ("secrets/runtime.password", b" private-password\n".to_vec()),
    ];
    for (path, bytes) in &inputs {
        std::fs::write(state.path().join(path), bytes).expect("write fixture input");
    }
    let valid = json!([{"Id":container,"State":{"Running":true},"Config":{"Labels":{"com.completedottech.codex.pg.instance":instance,"com.docker.compose.project":"codex-pg-fixture"}},"NetworkSettings":{"Ports":{"5432/tcp":[{"HostIp":"127.0.0.1","HostPort":"15432"}]}}}]);
    for (pointer, replacement, expected) in [
        ("", json!(null), None),
        (
            "engine",
            json!("foreign-engine"),
            Some("fixture_engine_mismatch"),
        ),
        (
            "/0/Id",
            json!("b".repeat(64)),
            Some("fixture_container_mismatch"),
        ),
        (
            "/0/Config/Labels/com.docker.compose.project",
            json!("codex-pg-foreign"),
            Some("fixture_container_mismatch"),
        ),
        (
            "/0/Config/Labels/com.completedottech.codex.pg.instance",
            json!("foreign"),
            Some("fixture_container_mismatch"),
        ),
        (
            "/0/State/Running",
            json!(false),
            Some("fixture_container_mismatch"),
        ),
        (
            "/0/NetworkSettings/Ports/5432~1tcp/0/HostIp",
            json!("0.0.0.0"),
            Some("fixture_binding_mismatch"),
        ),
        (
            "/0/NetworkSettings/Ports/5432~1tcp/0/HostPort",
            json!("15433"),
            Some("fixture_binding_mismatch"),
        ),
    ] {
        let mut actual = valid.clone();
        let engine = if pointer == "engine" {
            "foreign-engine"
        } else {
            "engine"
        };
        if !pointer.is_empty() && pointer != "engine" {
            *actual.pointer_mut(pointer).expect("causal fixture field") = replacement;
        }
        let calls = Arc::new(AtomicUsize::new(0));
        let observed = calls.clone();
        let result = settings_with_inspection(state.path(), "runtime", move |context, args| {
            let call = observed.fetch_add(1, Ordering::SeqCst);
            assert_eq!(context, "fixture");
            let stdout = match call {
                0 => {
                    assert_eq!(args, vec!["info", "--format", "{{.ID}}"]);
                    engine.as_bytes().to_vec()
                }
                1 => {
                    assert_eq!(args, vec!["inspect".to_string(), "a".repeat(64)]);
                    serde_json::to_vec(&actual).expect("bounded inspection")
                }
                _ => panic!("unexpected inspection"),
            };
            async move {
                Ok(std::process::Output {
                    status: std::process::ExitStatus::from_raw(/*raw*/ 0),
                    stdout,
                    stderr: Vec::new(),
                })
            }
        })
        .await;
        assert_eq!(
            calls.load(Ordering::SeqCst),
            if pointer == "engine" { 1 } else { 2 }
        );
        match expected {
            Some(code) => assert_eq!(result.err(), Some(code)),
            None => {
                let settings = result.expect("complete valid attestation");
                assert_eq!(
                    (
                        settings.host,
                        settings.port,
                        settings.database,
                        settings.username,
                        settings.password.expose(),
                        settings.ca_certificate,
                        settings.limits.connect_timeout,
                        settings.limits.acquire_timeout,
                        settings.limits.max_connections
                    ),
                    (
                        "localhost".to_string(),
                        15432,
                        "codex".to_string(),
                        "codex_runtime".to_string(),
                        "private-password",
                        state.path().join("secrets/ca.crt"),
                        Duration::from_secs(/*secs*/ 5),
                        Duration::from_secs(/*secs*/ 5),
                        3
                    )
                );
            }
        }
        let retained: Vec<_> = inputs
            .iter()
            .map(|(path, _)| {
                (
                    *path,
                    std::fs::read(state.path().join(path)).expect("preserved fixture input"),
                )
            })
            .collect();
        assert_eq!(retained, inputs.to_vec());
    }
}
