//! Inactive, artifact-bound input for PostgreSQL rollback preflight.
//!
//! The publisher key must come from a separately verified installation or
//! release policy. This module does not select an executable or approve a
//! storage cutover. Recheck the artifact immediately before any future swap.

use crate::ClientCapabilities;
use crate::CompatibilityError;
use crate::CompatibilityResult;
use crate::NamedNamespace;
use crate::PostgresPool;
use crate::RequiredAccess;
use crate::check_codex_storage_compatibility;
use crate::check_named_namespace_compatibility;
use ed25519_dalek::Signature;
use ed25519_dalek::Verifier as _;
use ed25519_dalek::VerifyingKey;
use serde::Deserialize;
use sha2::Digest as _;
use sha2::Sha256;
use std::fmt;
use std::fs::File;
use std::io::Read;
use std::path::Path;

const MANIFEST_CONTEXT: &[u8] = b"codex-storage-target-manifest-v1\0";
const MANIFEST_LIMIT: usize = 4096;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SignedManifest {
    kind: String,
    product: String,
    release: String,
    artifact_sha256: String,
    min_schema_format: i32,
    max_schema_format: i32,
    reader_version: i32,
    writer_version: i32,
}

/// A target whose signed capabilities match bytes read from an artifact.
/// It is only an input to read-only preflight, never activation authority.
pub struct VerifiedTarget {
    capabilities: ClientCapabilities,
    release: String,
    artifact_sha256: [u8; 32],
}

impl VerifiedTarget {
    /// The signed fork release identity, for a redacted receipt.
    pub fn release(&self) -> &str {
        &self.release
    }

    /// The digest checked at verification time. Recheck before any later swap.
    pub fn artifact_sha256(&self) -> [u8; 32] {
        self.artifact_sha256
    }
}

/// A target that cannot be trusted for even an inactive compatibility check.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TargetVerificationError {
    InvalidManifest,
    InvalidSignature,
    InvalidPublisher,
    InvalidArtifact,
}

impl fmt::Display for TargetVerificationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PostgreSQL target verification error: {self:?}")
    }
}

impl std::error::Error for TargetVerificationError {}

/// Verify a detached Ed25519 signature over the exact manifest bytes and the
/// artifact's SHA-256 digest. The trusted key is supplied by installation
/// policy, never from this manifest or the artifact being checked.
pub fn verify_target_artifact(
    manifest_bytes: &[u8],
    signature_bytes: &[u8],
    artifact: &Path,
    trusted_publisher_key: &[u8; 32],
) -> Result<VerifiedTarget, TargetVerificationError> {
    if manifest_bytes.is_empty() || manifest_bytes.len() > MANIFEST_LIMIT {
        return Err(TargetVerificationError::InvalidManifest);
    }
    let key = VerifyingKey::from_bytes(trusted_publisher_key)
        .map_err(|_| TargetVerificationError::InvalidPublisher)?;
    let signature = Signature::from_slice(signature_bytes)
        .map_err(|_| TargetVerificationError::InvalidSignature)?;
    let mut signed = Vec::with_capacity(MANIFEST_CONTEXT.len() + manifest_bytes.len());
    signed.extend_from_slice(MANIFEST_CONTEXT);
    signed.extend_from_slice(manifest_bytes);
    key.verify(&signed, &signature)
        .map_err(|_| TargetVerificationError::InvalidSignature)?;

    let manifest: SignedManifest = serde_json::from_slice(manifest_bytes)
        .map_err(|_| TargetVerificationError::InvalidManifest)?;
    if manifest.kind != "codex-storage-target-v1"
        || manifest.product != "CompleteDotTech/codex"
        || manifest.release.is_empty()
        || manifest.release.len() > 128
        || !manifest
            .release
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_' | b'+'))
        || manifest.min_schema_format <= 0
        || manifest.max_schema_format < manifest.min_schema_format
        || manifest.reader_version <= 0
        || manifest.writer_version <= 0
    {
        return Err(TargetVerificationError::InvalidManifest);
    }
    let digest =
        decode_digest(&manifest.artifact_sha256).ok_or(TargetVerificationError::InvalidManifest)?;
    let mut file = File::open(artifact).map_err(|_| TargetVerificationError::InvalidArtifact)?;
    if !file
        .metadata()
        .map_err(|_| TargetVerificationError::InvalidArtifact)?
        .is_file()
    {
        return Err(TargetVerificationError::InvalidArtifact);
    }
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 8192];
    loop {
        let count = file
            .read(&mut buffer)
            .map_err(|_| TargetVerificationError::InvalidArtifact)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    if hasher.finalize().as_slice() != digest {
        return Err(TargetVerificationError::InvalidArtifact);
    }
    Ok(VerifiedTarget {
        capabilities: ClientCapabilities {
            min_schema_format: manifest.min_schema_format,
            max_schema_format: manifest.max_schema_format,
            reader_version: manifest.reader_version,
            writer_version: manifest.writer_version,
        },
        release: manifest.release,
        artifact_sha256: digest,
    })
}

fn decode_digest(value: &str) -> Option<[u8; 32]> {
    let bytes = value.as_bytes();
    if bytes.len() != 64 {
        return None;
    }
    let mut digest = [0_u8; 32];
    for (index, pair) in bytes.chunks_exact(2).enumerate() {
        let high = (pair[0] as char).to_digit(16)? as u8;
        let low = (pair[1] as char).to_digit(16)? as u8;
        digest[index] = high << 4 | low;
    }
    Some(digest)
}

/// Check a fixed namespace for an artifact-bound target. Success is still a
/// prefilter result with `activation_permitted == false`.
pub async fn check_verified_target_compatibility(
    migrator: &PostgresPool,
    target: &VerifiedTarget,
    access: RequiredAccess,
) -> Result<CompatibilityResult, CompatibilityError> {
    check_codex_storage_compatibility(migrator, target.capabilities, access).await
}

/// Check a named namespace for an artifact-bound target. Success does not
/// qualify the native target or authorize an executable rollback.
pub async fn check_verified_named_target_compatibility(
    migrator: &PostgresPool,
    namespace: &NamedNamespace,
    target: &VerifiedTarget,
    access: RequiredAccess,
) -> Result<CompatibilityResult, CompatibilityError> {
    check_named_namespace_compatibility(migrator, namespace, target.capabilities, access).await
}

#[cfg(test)]
#[path = "verified_target_tests.rs"]
pub(crate) mod tests;
