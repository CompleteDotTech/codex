use super::*;
use crate::ConfigRequirements;
use crate::ConfigRequirementsToml;
use pretty_assertions::assert_eq;

fn layer(name: ConfigLayerSource, candidate: &str) -> ConfigLayerEntry {
    ConfigLayerEntry::new(name, toml::from_str(candidate).unwrap())
}

fn stack(layers: Vec<ConfigLayerEntry>) -> io::Result<ConfigLayerStack> {
    ConfigLayerStack::new(
        layers,
        ConfigRequirements::default(),
        ConfigRequirementsToml::default(),
    )
}

const LOCAL: &str = "[storage_candidate]\nbackend = 'local_sqlite'\n";
const REMOTE: &str = "[storage_candidate]\nbackend = 'remote_postgres'\nendpoint = 'db.example.test'\nport = 5432\ndatabase = 'codex'\nnamespace = 'history'\nconnect_timeout_seconds = 5\npool_acquire_timeout_seconds = 5\nmax_connections = 8\n[storage_candidate.credential]\nsource = 'environment'\nvariable = 'CODEX_PG_PASSWORD'\n";

#[test]
fn trusted_precedence_selects_complete_candidate_without_activating_it() {
    let user_stack = stack(vec![
        layer(
            ConfigLayerSource::System {
                file: std::env::current_exe().unwrap().try_into().unwrap(),
            },
            REMOTE,
        ),
        layer(
            ConfigLayerSource::User {
                file: std::env::current_exe().unwrap().try_into().unwrap(),
                profile: None,
            },
            LOCAL,
        ),
    ])
    .unwrap();
    assert_eq!(
        user_stack.storage_candidate().unwrap(),
        Some(StorageCandidateProfile::LocalSqlite)
    );
    let effective: crate::config_toml::ConfigToml =
        user_stack.effective_config().try_into().unwrap();
    assert_eq!(
        effective.storage_candidate,
        user_stack.storage_candidate().unwrap()
    );
    assert_eq!(effective.sqlite_home, None);

    let managed = stack(vec![
        layer(
            ConfigLayerSource::User {
                file: std::env::current_exe().unwrap().try_into().unwrap(),
                profile: None,
            },
            LOCAL,
        ),
        layer(ConfigLayerSource::LegacyManagedConfigTomlFromMdm, REMOTE),
    ])
    .unwrap();
    assert!(matches!(
        managed.storage_candidate().unwrap(),
        Some(StorageCandidateProfile::RemotePostgres(_))
    ));
}

#[test]
fn project_and_session_cannot_supply_candidate() {
    let project = layer(
        ConfigLayerSource::Project {
            dot_codex_folder: std::env::current_exe().unwrap().try_into().unwrap(),
        },
        REMOTE,
    );
    for untrusted in [project, layer(ConfigLayerSource::SessionFlags, REMOTE)] {
        let error = stack(vec![untrusted]).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert!(!error.to_string().contains("db.example.test"));
    }
}

#[test]
fn invalid_trusted_profile_error_is_redacted() {
    let error = stack(vec![layer(
        ConfigLayerSource::LegacyManagedConfigTomlFromMdm,
        "[storage_candidate]\nbackend = 'remote_postgres'\nendpoint = 'secret.example.test'\n",
    )])
    .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert!(!error.to_string().contains("secret.example.test"));
}

#[test]
fn escaped_candidate_keys_redact_malformed_source_diagnostics() {
    for encoded_key in [r"storage_\u0063andidate", r"storage_\U00000063andidate"] {
        let valid = format!("[\"{encoded_key}\"]\nbackend = 'local_sqlite'\n");
        let decoded: TomlValue = toml::from_str(&valid).unwrap();
        assert_eq!(decoded, toml::from_str::<TomlValue>(LOCAL).unwrap());

        let malformed = format!("[\"{encoded_key}\"]\nendpoint = 'secret.example.test\n");
        let parser_error = toml::from_str::<TomlValue>(&malformed).unwrap_err();
        assert!(parser_error.to_string().contains("secret.example.test"));
        let redacted = redacted_parse_error(&malformed).unwrap();
        assert_eq!(redacted.kind(), io::ErrorKind::InvalidData);
        assert_eq!(redacted.to_string(), "invalid configuration");
        let cloud = crate::cloud_config_layers_from_fragments(
            [crate::CloudConfigFragment {
                id: "id".into(),
                name: "host".into(),
                contents: malformed,
            }],
            &std::env::current_dir().unwrap().try_into().unwrap(),
        )
        .unwrap_err();
        assert!(
            matches!(cloud, crate::CloudConfigLayerError::Parse { message, .. } if message == "invalid configuration")
        );
    }
}

#[test]
fn ordinary_malformed_config_keeps_source_diagnostics() {
    let malformed = "model = 'ordinary.example.test\n";
    assert!(redacted_parse_error(malformed).is_none());
    let parser_error = toml::from_str::<TomlValue>(malformed).unwrap_err();
    assert!(parser_error.to_string().contains("ordinary.example.test"));
}
