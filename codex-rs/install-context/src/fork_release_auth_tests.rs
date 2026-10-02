//! Synthetic signing keys prove provenance, replay and payload refusal; no release is published.
use super::*;
use ed25519_dalek::Signer;
use ed25519_dalek::SigningKey;
use pretty_assertions::assert_eq;

fn payload(key: &SigningKey) -> serde_json::Value {
    serde_json::json!({"protocolVersion":1,"algorithm":"ed25519", "keyId":format!("ed25519:{:x}",Sha256::digest(key.verifying_key().to_bytes())),
        "owner":"CompleteDotTech/codex","channel":"stable","target":"fixture-linux","variant":"full","packageVersion":"1.2.3",
        "declaredBaseCommit":"a".repeat(40),"forkCommit":"b".repeat(40),"patchsetSha256":format!("sha256:{}","c".repeat(64)),
        "manifestSha256":format!("sha256:{:x}",Sha256::digest(b"fixture manifest")),"archiveSha256":format!("sha256:{:x}",Sha256::digest(b"fixture archive")),
        "archiveBytes":15,"storageCapabilities":["sqlite"],"postgresSchemaVersions":[],"minimumReaderSchema":0,"minimumWriterSchema":0,"releaseSequence":2})
}

fn signature(key: &SigningKey, bytes: &[u8]) -> [u8; 64] {
    let mut signed = DOMAIN.to_vec();
    signed.extend_from_slice(bytes);
    key.sign(&signed).to_bytes()
}

#[test]
fn exact_signed_descriptor_and_held_payload_verify() {
    let key = SigningKey::from_bytes(&[17; 32]);
    let bytes = serde_json::to_vec(&payload(&key)).unwrap();
    let capabilities = vec!["sqlite".to_owned()];
    let base = "a".repeat(40);
    let requirements = ForkReleaseRequirements {
        channel: "stable",
        target: "fixture-linux",
        variant: "full",
        declared_base_commit: &base,
        storage_capabilities: &capabilities,
        postgres_schema_versions: &[],
        reader_schema: 0,
        writer_schema: 0,
        accepted_release_sequence: 1,
        maximum_archive_bytes: 1024,
    };
    let verifier =
        ConfiguredForkReleaseVerifier::from_owner_configuration(key.verifying_key().to_bytes())
            .unwrap();
    let release = verifier
        .verify(&bytes, &signature(&key, &bytes), &requirements)
        .unwrap();
    assert_eq!(
        (
            release.package_version(),
            release.fork_commit(),
            release.release_sequence()
        ),
        ("1.2.3", "b".repeat(40).as_str(), 2)
    );
    release.verify_manifest_bytes(b"fixture manifest").unwrap();
    assert!(release.verify_manifest_bytes(b"tampered manifest").is_err());
    let mut file = tempfile::tempfile().unwrap();
    std::io::Write::write_all(&mut file, b"fixture archive").unwrap();
    assert!(release.verify_archive(file).is_ok());
    let mut file = tempfile::tempfile().unwrap();
    std::io::Write::write_all(&mut file, b"wrong archive!!").unwrap();
    assert!(release.verify_archive(file).is_err());
}

#[test]
fn wrong_key_signature_raw_duplicate_channel_and_schema_claims_refuse() {
    let key = SigningKey::from_bytes(&[17; 32]);
    let foreign = SigningKey::from_bytes(&[18; 32]);
    let base = "a".repeat(40);
    let capabilities = vec!["sqlite".to_owned()];
    let requirements = ForkReleaseRequirements {
        channel: "stable",
        target: "fixture-linux",
        variant: "full",
        declared_base_commit: &base,
        storage_capabilities: &capabilities,
        postgres_schema_versions: &[],
        reader_schema: 0,
        writer_schema: 0,
        accepted_release_sequence: 1,
        maximum_archive_bytes: 1024,
    };
    let verifier =
        ConfiguredForkReleaseVerifier::from_owner_configuration(key.verifying_key().to_bytes())
            .unwrap();
    let original = serde_json::to_vec(&payload(&key)).unwrap();
    assert!(
        verifier
            .verify(&original, &signature(&foreign, &original), &requirements)
            .is_err()
    );
    assert!(verifier.verify(&original, &[0; 64], &requirements).is_err());
    for (field, bad_value) in [
        ("channel", serde_json::json!("preview")),
        ("owner", serde_json::json!("openai/codex")),
        ("target", serde_json::json!("other-host")),
        ("variant", serde_json::json!("other")),
        ("releaseSequence", serde_json::json!(1)),
        ("minimumReaderSchema", serde_json::json!(1)),
        ("minimumWriterSchema", serde_json::json!(1)),
        ("protocolVersion", serde_json::json!(2)),
        (
            "lifecycleProtocol",
            serde_json::json!({"runtimeFenceVersion":1,"registrationVersion":1,"schemaContractSha256":format!("sha256:{}","e".repeat(64))}),
        ),
        ("archiveBytes", serde_json::json!(1025)),
        ("futureAuthority", serde_json::json!("ignored")),
    ] {
        let mut value = payload(&key);
        value[field] = bad_value;
        let bytes = serde_json::to_vec(&value).unwrap();
        assert!(
            verifier
                .verify(&bytes, &signature(&key, &bytes), &requirements)
                .is_err()
        );
    }
    let duplicate = format!(
        "{{\"releaseSequence\":99,{}",
        String::from_utf8(original.clone())
            .unwrap()
            .trim_start_matches('{')
    );
    assert!(
        verifier
            .verify(
                duplicate.as_bytes(),
                &signature(&key, duplicate.as_bytes()),
                &requirements
            )
            .is_err()
    );
    let mut changed = original.clone();
    changed.push(b' ');
    assert!(
        verifier
            .verify(&changed, &signature(&key, &original), &requirements)
            .is_err()
    );
}

#[test]
fn equal_numeric_version_different_fork_commit_is_distinct_authenticated_candidate() {
    let key = SigningKey::from_bytes(&[17; 32]);
    let base = "a".repeat(40);
    let capabilities = vec!["sqlite".to_owned()];
    let requirements = ForkReleaseRequirements {
        channel: "stable",
        target: "fixture-linux",
        variant: "full",
        declared_base_commit: &base,
        storage_capabilities: &capabilities,
        postgres_schema_versions: &[],
        reader_schema: 0,
        writer_schema: 0,
        accepted_release_sequence: 1,
        maximum_archive_bytes: 1024,
    };
    let verifier =
        ConfiguredForkReleaseVerifier::from_owner_configuration(key.verifying_key().to_bytes())
            .unwrap();
    let first = serde_json::to_vec(&payload(&key)).unwrap();
    let mut second = payload(&key);
    second["forkCommit"] = serde_json::json!("d".repeat(40));
    let second = serde_json::to_vec(&second).unwrap();
    let first = verifier
        .verify(&first, &signature(&key, &first), &requirements)
        .unwrap();
    let second = verifier
        .verify(&second, &signature(&key, &second), &requirements)
        .unwrap();
    assert_eq!(first.package_version(), second.package_version());
    assert_ne!(first.fork_commit(), second.fork_commit());
}
