//! Storage candidate proposals remain private and read-only through public RPCs.

use anyhow::Result;
use app_test_support::TestAppServer;
use codex_app_server_protocol::ConfigBatchWriteParams;
use codex_app_server_protocol::ConfigEdit;
use codex_app_server_protocol::ConfigReadParams;
use codex_app_server_protocol::ConfigReadResponse;
use codex_app_server_protocol::ConfigValueWriteParams;
use codex_app_server_protocol::MergeStrategy;
use codex_app_server_protocol::RequestId;
use pretty_assertions::assert_eq;
use serde_json::json;
use tempfile::TempDir;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn storage_candidate_is_hidden_and_readonly_over_public_config_rpc() -> Result<()> {
    let home = TempDir::new()?;
    let path = home.path().join("config.toml");
    let candidate = "[storage_candidate]\nbackend = 'remote_postgres'\nendpoint = 'db.secret.test'\nport = 5432\ndatabase = 'codex'\nnamespace = 'history'\nconnect_timeout_seconds = 5\npool_acquire_timeout_seconds = 5\nmax_connections = 8\n[storage_candidate.credential]\nsource = 'environment'\nvariable = 'SECRET_PASSWORD_ENV'\n";
    std::fs::write(&path, candidate)?;
    let mut server = TestAppServer::builder()
        .with_codex_home(home.path())
        .build()
        .await?;
    server.initialize().await?;
    let id = server
        .send_config_read_request(ConfigReadParams {
            include_layers: true,
            cwd: None,
        })
        .await?;
    let response: ConfigReadResponse = server.read_response(id).await?;
    let wire = serde_json::to_string(&response)?;
    assert!(!wire.contains("db.secret.test"));
    assert!(!wire.contains("SECRET_PASSWORD_ENV"));
    assert!(!response.config.additional.contains_key("storage_candidate"));
    assert!(
        response
            .origins
            .keys()
            .all(|key| !key.starts_with("storage_candidate"))
    );
    assert!(response.layers.unwrap().iter().all(|layer| {
        !layer
            .config
            .as_object()
            .is_some_and(|config| config.contains_key("storage_candidate"))
    }));
    for key_path in ["storage_candidate", "storage_candidate.endpoint"] {
        let id = server
            .send_config_value_write_request(ConfigValueWriteParams {
                file_path: Some(path.display().to_string()),
                key_path: key_path.into(),
                value: json!("attacker.example"),
                merge_strategy: MergeStrategy::Replace,
                expected_version: None,
            })
            .await?;
        let error = server
            .read_stream_until_error_message(RequestId::Integer(id))
            .await?;
        assert_eq!(
            error.error.data,
            Some(json!({"config_write_error_code": "configLayerReadonly"}))
        );
    }
    let id = server
        .send_config_batch_write_request(ConfigBatchWriteParams {
            edits: vec![ConfigEdit {
                key_path: "storage_candidate.endpoint".into(),
                value: json!("attacker.example"),
                merge_strategy: MergeStrategy::Replace,
            }],
            file_path: Some(path.display().to_string()),
            expected_version: None,
            reload_user_config: false,
        })
        .await?;
    let error = server
        .read_stream_until_error_message(RequestId::Integer(id))
        .await?;
    assert_eq!(
        error.error.data,
        Some(json!({"config_write_error_code": "configLayerReadonly"}))
    );
    assert_eq!(std::fs::read_to_string(&path)?, candidate);
    for key in [
        "storage_candidate",
        r"storage_\u0063andidate",
        r"storage_\U00000063andidate",
    ] {
        std::fs::write(
            &path,
            format!("[\"{key}\"]\nendpoint = 'db.secret.test\nbroken = ["),
        )?;
        let id = server
            .send_config_read_request(ConfigReadParams {
                include_layers: true,
                cwd: None,
            })
            .await?;
        let error = server
            .read_stream_until_error_message(RequestId::Integer(id))
            .await?;
        assert!(!serde_json::to_string(&error)?.contains("db.secret.test"));
    }
    Ok(())
}
