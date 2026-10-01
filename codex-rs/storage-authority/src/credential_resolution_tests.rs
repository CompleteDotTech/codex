use super::*;
use crate::CredentialRef;
use crate::EnvironmentVariableName;
use codex_keyring_store::tests::MockKeyringStore;
use pretty_assertions::assert_eq;

fn environment_source() -> CredentialSource {
    CredentialSource::Environment {
        variable: EnvironmentVariableName::parse("CODEX_TEST_DB_PASSWORD".to_string()).unwrap(),
    }
}

fn keyring_source() -> CredentialSource {
    CredentialSource::Keyring {
        id: CredentialRef::parse("test-database".to_string()).unwrap(),
    }
}

#[test]
fn environment_lookup_is_fresh_and_does_not_read_unrelated_values() {
    let keyring = MockKeyringStore::default();
    let resolver = HostCredentialResolver::new(&keyring);
    let source = environment_source();
    let first = resolver
        .resolve_with_environment(&source, |name| {
            assert_eq!(name, "CODEX_TEST_DB_PASSWORD");
            Ok(Some("first-secret".to_string()))
        })
        .unwrap();
    let rotated = resolver
        .resolve_with_environment(&source, |_| Ok(Some("rotated-secret".to_string())))
        .unwrap();
    assert_eq!(first.expose(), "first-secret");
    assert_eq!(rotated.expose(), "rotated-secret");
    assert!(!keyring.contains("test-database"));
    assert_eq!(format!("{first:?}"), "ResolvedCredential([redacted])");
}

#[test]
fn keyring_lookup_uses_dedicated_service_and_sees_rotation() {
    let keyring = MockKeyringStore::default();
    keyring
        .save(KEYRING_SERVICE, "test-database", "first-secret")
        .unwrap();
    let resolver = HostCredentialResolver::new(&keyring);
    let source = keyring_source();
    assert_eq!(resolver.resolve(&source).unwrap().expose(), "first-secret");
    keyring
        .save(KEYRING_SERVICE, "test-database", "rotated-secret")
        .unwrap();
    assert_eq!(
        resolver.resolve(&source).unwrap().expose(),
        "rotated-secret"
    );
}

#[test]
fn missing_and_invalid_values_remain_redacted() {
    let keyring = MockKeyringStore::default();
    let resolver = HostCredentialResolver::new(&keyring);
    let source = environment_source();
    assert_eq!(
        resolver
            .resolve_with_environment(&source, |_| Ok(None))
            .err(),
        Some(CredentialResolutionError::Missing)
    );
    assert_eq!(
        resolver
            .resolve_with_environment(&source, |_| Ok(Some(String::new())))
            .err(),
        Some(CredentialResolutionError::InvalidValue)
    );
    assert_eq!(
        resolver
            .resolve_with_environment(&source, |_| Ok(Some("x".repeat(MAX_CREDENTIAL_BYTES + 1))))
            .err(),
        Some(CredentialResolutionError::InvalidValue)
    );
    assert_eq!(
        resolver
            .resolve_with_environment(&source, |_| Err(CredentialResolutionError::InvalidValue))
            .err(),
        Some(CredentialResolutionError::InvalidValue)
    );
    let keyring_source = keyring_source();
    assert_eq!(
        resolver.resolve(&keyring_source).err(),
        Some(CredentialResolutionError::Missing)
    );
    for error in [
        CredentialResolutionError::Missing,
        CredentialResolutionError::InvalidValue,
        CredentialResolutionError::StoreUnavailable,
    ] {
        let output = format!("{error:?} {error}");
        assert!(!output.contains("CODEX_TEST_DB_PASSWORD"));
        assert!(!output.contains("test-database"));
        assert!(!output.contains("secret"));
    }
}
