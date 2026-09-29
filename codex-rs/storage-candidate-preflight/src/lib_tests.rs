use super::*;
use codex_config::ConfigLayerEntry;
use codex_config::ConfigLayerSource;
use codex_config::ConfigRequirements;
use codex_config::ConfigRequirementsToml;
use codex_keyring_store::KeyringStore;
use codex_keyring_store::tests::MockKeyringStore;
use codex_postgres_runtime::verify_target_artifact;
use ed25519_dalek::Signer as _;
use ed25519_dalek::SigningKey;
use pretty_assertions::assert_eq;
use serde_json::json;
use sha2::Digest as _;
use sha2::Sha256;
use std::path::Path;

const SERVICE: &str = "codex-postgres-storage";
const ACCOUNT: &str = "bridge_fixture";

fn signed_target() -> (tempfile::TempDir, VerifiedTarget) {
    let directory = tempfile::tempdir().unwrap();
    let artifact = directory.path().join("fixture-codex");
    let contents = b"test-only codex binary";
    std::fs::write(&artifact, contents).unwrap();
    let digest = Sha256::digest(contents);
    let manifest = serde_json::to_vec(&json!({
        "kind": "codex-storage-target-v1",
        "product": "CompleteDotTech/codex",
        "release": "fixture-v5",
        "artifact_sha256": format!("{digest:x}"),
        "min_schema_format": 5,
        "max_schema_format": 5,
        "reader_version": 5,
        "writer_version": 5,
    }))
    .unwrap();
    let key = SigningKey::from_bytes(&[7_u8; 32]);
    let mut signed = b"codex-storage-target-manifest-v1\0".to_vec();
    signed.extend_from_slice(&manifest);
    let signature = key.sign(&signed).to_bytes();
    let target = verify_target_artifact(
        &manifest,
        &signature,
        &artifact,
        &key.verifying_key().to_bytes(),
    )
    .unwrap();
    (directory, target)
}

fn remote(port: u16, namespace: &str, ca: Option<&Path>) -> String {
    let ca = ca.map_or(String::new(), |path| {
        format!(
            "ca_certificate = '{}'\n",
            path.to_string_lossy().replace('\\', "/")
        )
    });
    format!(
        "[storage_candidate]\nbackend = 'remote_postgres'\nendpoint = 'localhost'\nport = {port}\ndatabase = 'codex'\nnamespace = '{namespace}'\nconnect_timeout_seconds = 5\npool_acquire_timeout_seconds = 5\nmax_connections = 2\n[storage_candidate.credential]\nsource = 'keyring'\nid = '{ACCOUNT}'\n[storage_candidate.tls]\n{ca}"
    )
}

fn stack(candidate: &str, source: ConfigLayerSource) -> Result<ConfigLayerStack, std::io::Error> {
    ConfigLayerStack::new(
        vec![ConfigLayerEntry::new(
            source,
            toml::from_str(candidate).unwrap(),
        )],
        ConfigRequirements::default(),
        ConfigRequirementsToml::default(),
    )
}

fn user_source() -> ConfigLayerSource {
    ConfigLayerSource::User {
        file: std::env::current_exe().unwrap().try_into().unwrap(),
        profile: None,
    }
}

#[tokio::test]
async fn unsupported_and_missing_inputs_fail_before_secret_lookup_or_network() {
    let (_artifact, target) = signed_target();
    let keyring = MockKeyringStore::default();
    let resolver = HostCredentialResolver::new(&keyring);
    let ca = tempfile::tempdir().unwrap();
    let candidate = remote(1, "history", Some(&ca.path().join("ca.crt")));
    assert_eq!(
        preflight_trusted_candidate(
            &stack(&candidate, user_source()).unwrap(),
            &resolver,
            &target
        )
        .await,
        Err(CandidatePreflightError::UnsupportedNamespace)
    );
    let candidate = remote(1, "codex_storage_bridge", None);
    assert_eq!(
        preflight_trusted_candidate(
            &stack(&candidate, user_source()).unwrap(),
            &resolver,
            &target
        )
        .await,
        Err(CandidatePreflightError::ExplicitCaRequired)
    );
    let candidate = remote(1, "codex_storage_bridge", Some(&ca.path().join("ca.crt")));
    assert_eq!(
        preflight_trusted_candidate(
            &stack(&candidate, user_source()).unwrap(),
            &resolver,
            &target
        )
        .await,
        Err(CandidatePreflightError::Credential(
            CredentialResolutionError::Missing
        ))
    );
    let project = ConfigLayerSource::Project {
        dot_codex_folder: std::env::current_exe().unwrap().try_into().unwrap(),
    };
    assert!(stack(&candidate, project).is_err());
    assert_eq!(
        preflight_trusted_candidate(
            &stack(
                "[storage_candidate]\nbackend = 'local_sqlite'",
                user_source()
            )
            .unwrap(),
            &resolver,
            &target,
        )
        .await,
        Err(CandidatePreflightError::NoRemoteCandidate)
    );
}

#[tokio::test]
async fn wrong_credential_reports_only_a_redacted_connection_error() {
    let Ok(state) = std::env::var("CODEX_TEST_POSTGRES_ISOLATION_STATE") else {
        return;
    };
    let state = Path::new(&state);
    let (_artifact, target) = signed_target();
    let receipt: serde_json::Value =
        serde_json::from_slice(&std::fs::read(state.join("receipt.json")).unwrap()).unwrap();
    let port = receipt["port"].as_u64().unwrap() as u16;
    let candidate = remote(
        port,
        "codex_storage_isolation",
        Some(&state.join("secrets/ca.crt")),
    );
    let keyring = MockKeyringStore::default();
    keyring
        .save(SERVICE, ACCOUNT, "wrong-secret-bridge-probe")
        .unwrap();
    let resolver = HostCredentialResolver::new(&keyring);
    let error = preflight_trusted_candidate(
        &stack(&candidate, user_source()).unwrap(),
        &resolver,
        &target,
    )
    .await
    .unwrap_err();
    assert_eq!(
        error,
        CandidatePreflightError::Connection(PoolError::Authentication)
    );
    assert!(!format!("{error:?} {error}").contains("wrong-secret-bridge-probe"));
}

#[tokio::test]
async fn preprovisioned_named_namespace_is_checked_without_activation() {
    let Ok(state) = std::env::var("CODEX_TEST_POSTGRES_ISOLATION_STATE") else {
        return;
    };
    let state = Path::new(&state);
    let (_artifact, target) = signed_target();
    let receipt: serde_json::Value =
        serde_json::from_slice(&std::fs::read(state.join("receipt.json")).unwrap()).unwrap();
    let port = receipt["port"].as_u64().unwrap() as u16;
    let candidate = remote(
        port,
        "codex_storage_isolation",
        Some(&state.join("secrets/ca.crt")),
    );
    let keyring = MockKeyringStore::default();
    let secret =
        std::fs::read_to_string(state.join("secrets/isolation_migrator.password")).unwrap();
    keyring.save(SERVICE, ACCOUNT, secret.trim()).unwrap();
    let resolver = HostCredentialResolver::new(&keyring);
    let result = preflight_trusted_candidate(
        &stack(&candidate, user_source()).unwrap(),
        &resolver,
        &target,
    )
    .await
    .unwrap();
    assert_eq!(result.activation_permitted, false);
    assert_eq!(result.schema_format, 5);
}
