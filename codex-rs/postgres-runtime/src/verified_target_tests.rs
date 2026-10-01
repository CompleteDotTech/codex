use super::TargetVerificationError;
use super::VerifiedTarget;
use super::verify_target_artifact;
use ed25519_dalek::Signer as _;
use ed25519_dalek::SigningKey;
use pretty_assertions::assert_eq;
use serde_json::json;
use sha2::Digest as _;
use sha2::Sha256;
use std::path::PathBuf;

pub(crate) struct SignedFixture {
    _directory: tempfile::TempDir,
    artifact: PathBuf,
    manifest: Vec<u8>,
    signature: Vec<u8>,
    publisher: [u8; 32],
}

impl SignedFixture {
    pub(crate) fn new(format: i32) -> Self {
        let directory = tempfile::tempdir().expect("candidate directory");
        let artifact = directory.path().join("candidate-codex");
        let bytes = format!("test-only candidate artifact for format {format}");
        std::fs::write(&artifact, bytes.as_bytes()).expect("candidate artifact");
        let digest = Sha256::digest(bytes.as_bytes());
        let manifest = serde_json::to_vec(&json!({
            "kind": "codex-storage-target-v1",
            "product": "CompleteDotTech/codex",
            "release": format!("fixture-v{format}"),
            "artifact_sha256": format!("{digest:x}"),
            "min_schema_format": format,
            "max_schema_format": format,
            "reader_version": format,
            "writer_version": format,
        }))
        .expect("signed manifest");
        let key = SigningKey::from_bytes(&[7_u8; 32]);
        let mut signed = super::MANIFEST_CONTEXT.to_vec();
        signed.extend_from_slice(&manifest);
        let signature = key.sign(&signed).to_bytes().to_vec();
        Self {
            _directory: directory,
            artifact,
            manifest,
            signature,
            publisher: key.verifying_key().to_bytes(),
        }
    }

    pub(crate) fn verify(&self) -> VerifiedTarget {
        verify_target_artifact(
            &self.manifest,
            &self.signature,
            &self.artifact,
            &self.publisher,
        )
        .expect("signed fixture target")
    }
}

#[test]
fn rejects_unknown_unpatched_and_changed_artifacts() {
    let fixture = SignedFixture::new(4);
    assert_eq!(fixture.verify().release(), "fixture-v4");
    let unknown_key = SigningKey::from_bytes(&[8_u8; 32]);
    assert!(matches!(
        verify_target_artifact(
            &fixture.manifest,
            &fixture.signature,
            &fixture.artifact,
            &unknown_key.verifying_key().to_bytes(),
        ),
        Err(TargetVerificationError::InvalidSignature)
    ));
    let mut unpatched = fixture.manifest.clone();
    unpatched.extend_from_slice(b" ");
    assert!(matches!(
        verify_target_artifact(
            &unpatched,
            &fixture.signature,
            &fixture.artifact,
            &fixture.publisher,
        ),
        Err(TargetVerificationError::InvalidSignature)
    ));
    let mut upstream: serde_json::Value =
        serde_json::from_slice(&fixture.manifest).expect("fixture manifest");
    upstream["product"] = json!("OpenAI/codex");
    let upstream = serde_json::to_vec(&upstream).expect("upstream manifest");
    let mut signed = super::MANIFEST_CONTEXT.to_vec();
    signed.extend_from_slice(&upstream);
    let upstream_signature = SigningKey::from_bytes(&[7_u8; 32]).sign(&signed).to_bytes();
    assert!(matches!(
        verify_target_artifact(
            &upstream,
            &upstream_signature,
            &fixture.artifact,
            &fixture.publisher,
        ),
        Err(TargetVerificationError::InvalidManifest)
    ));
    std::fs::write(&fixture.artifact, b"replaced upstream executable")
        .expect("replace candidate artifact");
    assert!(matches!(
        verify_target_artifact(
            &fixture.manifest,
            &fixture.signature,
            &fixture.artifact,
            &fixture.publisher,
        ),
        Err(TargetVerificationError::InvalidArtifact)
    ));
}
