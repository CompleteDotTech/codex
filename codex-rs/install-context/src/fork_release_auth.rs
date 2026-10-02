//! Exact-byte signed fork release descriptors; never installer or migration authorization.
use ed25519_dalek::Signature;
use ed25519_dalek::VerifyingKey;
use serde::Deserialize;
use sha2::Digest;
use sha2::Sha256;
use std::fs::File;
use std::io;
use std::io::Read;
use std::io::Seek;
use std::io::SeekFrom;

const DOMAIN: &[u8] = b"CompleteDotTech/codex fork-release v1\0";
const DOMAIN_V2: &[u8] = b"CompleteDotTech/codex fork-release v2\0";
const MAX_DESCRIPTOR: usize = 16 * 1024;
const MAX_MANIFEST: usize = 4 * 1024 * 1024;

/// Must be supplied from protected owner configuration, never a downloaded candidate.
pub struct ConfiguredForkReleaseVerifier {
    key: VerifyingKey,
    key_id: String,
}

/// Compatibility and replay limits from the installed owner's policy and actual runtime.
pub struct ForkReleaseRequirements<'a> {
    pub channel: &'a str,
    pub target: &'a str,
    pub variant: &'a str,
    pub declared_base_commit: &'a str,
    pub storage_capabilities: &'a [String],
    pub postgres_schema_versions: &'a [u32],
    pub reader_schema: u32,
    pub writer_schema: u32,
    pub accepted_release_sequence: u64,
    pub maximum_archive_bytes: u64,
}

#[derive(Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ReleaseWire {
    protocol_version: u32,
    #[serde(default)]
    lifecycle_protocol: Option<LifecycleProtocolWire>,
    algorithm: String,
    key_id: String,
    owner: String,
    channel: String,
    target: String,
    variant: String,
    package_version: String,
    declared_base_commit: String,
    fork_commit: String,
    patchset_sha256: String,
    manifest_sha256: String,
    archive_sha256: String,
    archive_bytes: u64,
    storage_capabilities: Vec<String>,
    postgres_schema_versions: Vec<u32>,
    minimum_reader_schema: u32,
    minimum_writer_schema: u32,
    release_sequence: u64,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LifecycleProtocolWire {
    runtime_fence_version: u32,
    registration_version: u32,
    schema_contract_sha256: String,
}

/// Signed runtime protocol claims, not proof of legacy-process quiescence.
/// Only a verified v2 descriptor can construct this capability.
pub struct AuthenticatedRuntimeProtocol<'a>(&'a LifecycleProtocolWire);
impl AuthenticatedRuntimeProtocol<'_> {
    pub fn schema_contract_sha256(&self) -> &str {
        &self.0.schema_contract_sha256
    }
}

/// Authenticated descriptor claims only; payload, active receipt and ownership still need verification.
pub struct AuthenticatedForkRelease(ReleaseWire);

/// Owns the held archive descriptor after size/digest verification; no activation API is exposed.
pub struct VerifiedForkArchive {
    _archive: File,
}

fn invalid() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "invalid signed fork release")
}

impl ConfiguredForkReleaseVerifier {
    /// Configuration authenticity and private host ownership must already be established by caller.
    pub fn from_owner_configuration(public_key: [u8; 32]) -> io::Result<Self> {
        let key = VerifyingKey::from_bytes(&public_key).map_err(|_| invalid())?;
        let key_id = format!("ed25519:{:x}", Sha256::digest(public_key));
        Ok(Self { key, key_id })
    }

    pub fn verify(
        &self,
        descriptor: &[u8],
        signature: &[u8],
        requirements: &ForkReleaseRequirements<'_>,
    ) -> io::Result<AuthenticatedForkRelease> {
        if descriptor.len() > MAX_DESCRIPTOR {
            return Err(invalid());
        }
        // Parse bounded original bytes once, but expose no claims before signature verification.
        let release: ReleaseWire = serde_json::from_slice(descriptor).map_err(|_| invalid())?;
        let domain = match release.protocol_version {
            1 if release.lifecycle_protocol.is_none() => DOMAIN,
            2 if release.lifecycle_protocol.as_ref().is_some_and(|protocol| {
                protocol.runtime_fence_version == 1
                    && protocol.registration_version == 1
                    && sha_digest(&protocol.schema_contract_sha256)
            }) =>
            {
                DOMAIN_V2
            }
            _ => return Err(invalid()),
        };
        let signature = Signature::from_slice(signature).map_err(|_| invalid())?;
        let mut signed = Vec::with_capacity(domain.len() + descriptor.len());
        signed.extend_from_slice(domain);
        signed.extend_from_slice(descriptor);
        self.key
            .verify_strict(&signed, &signature)
            .map_err(|_| invalid())?;
        // Deserialize original bytes directly: duplicate/unknown authority never projects through Value.
        if release.algorithm != "ed25519"
            || release.key_id != self.key_id
            || release.owner != "CompleteDotTech/codex"
            || !matches!(release.channel.as_str(), "stable" | "preview")
            || release.channel != requirements.channel
            || release.target != requirements.target
            || release.variant != requirements.variant
            || release.declared_base_commit != requirements.declared_base_commit
            || release.storage_capabilities != requirements.storage_capabilities
            || release.postgres_schema_versions != requirements.postgres_schema_versions
            || release.storage_capabilities.len() > 32
            || release.postgres_schema_versions.len() > 32
            || release
                .storage_capabilities
                .iter()
                .any(|value| !bounded_text(value))
            || !bounded_text(&release.target)
            || !bounded_text(&release.variant)
            || semver::Version::parse(&release.package_version).is_err()
            || !hex_digest(&release.declared_base_commit, 40)
            || !hex_digest(&release.fork_commit, 40)
            || !sha_digest(&release.patchset_sha256)
            || !sha_digest(&release.manifest_sha256)
            || !sha_digest(&release.archive_sha256)
            || release.archive_bytes == 0
            || release.archive_bytes > requirements.maximum_archive_bytes
            || release.minimum_reader_schema > requirements.reader_schema
            || release.minimum_writer_schema > requirements.writer_schema
            || release.release_sequence <= requirements.accepted_release_sequence
        {
            return Err(invalid());
        }
        Ok(AuthenticatedForkRelease(release))
    }
}

impl AuthenticatedForkRelease {
    /// Requires the authenticated modern fencing protocol before registered-runtime eligibility.
    /// Payload inventory, schema-contract bytes, persisted identities and shutdown proof remain mandatory.
    pub fn runtime_protocol(&self) -> io::Result<AuthenticatedRuntimeProtocol<'_>> {
        if self.0.protocol_version != 2 {
            return Err(invalid());
        }
        self.0
            .lifecycle_protocol
            .as_ref()
            .map(AuthenticatedRuntimeProtocol)
            .ok_or_else(invalid)
    }

    pub fn fork_commit(&self) -> &str {
        &self.0.fork_commit
    }
    pub fn package_version(&self) -> &str {
        &self.0.package_version
    }
    pub fn release_sequence(&self) -> u64 {
        self.0.release_sequence
    }

    /// Integrity check only: full inventory, capability qualification and schema fencing remain mandatory.
    pub fn verify_manifest_bytes(&self, bytes: &[u8]) -> io::Result<()> {
        if bytes.len() > MAX_MANIFEST
            || format!("sha256:{:x}", Sha256::digest(bytes)) != self.0.manifest_sha256
        {
            return Err(invalid());
        }
        Ok(())
    }

    /// Hashes the held file, never reopens a downloaded path or allocates its declared size.
    pub fn verify_archive(&self, mut archive: File) -> io::Result<VerifiedForkArchive> {
        let before = archive.metadata()?;
        if !before.is_file() || before.len() != self.archive_bytes() {
            return Err(invalid());
        }
        archive.seek(SeekFrom::Start(0))?;
        let mut digest = Sha256::new();
        let mut read = 0u64;
        let mut buffer = [0u8; 65536];
        loop {
            let length = archive.read(&mut buffer)?;
            if length == 0 {
                break;
            }
            read = read.checked_add(length as u64).ok_or_else(invalid)?;
            if read > self.archive_bytes() {
                return Err(invalid());
            }
            digest.update(&buffer[..length]);
        }
        let after = archive.metadata()?;
        if read != before.len()
            || after.len() != before.len()
            || after.modified()? != before.modified()?
            || format!("sha256:{:x}", digest.finalize()) != self.archive_sha256()
        {
            return Err(invalid());
        }
        archive.seek(SeekFrom::Start(0))?;
        Ok(VerifiedForkArchive { _archive: archive })
    }
}

fn bounded_text(value: &str) -> bool {
    !value.is_empty() && value.len() <= 128 && !value.chars().any(char::is_control)
}
fn hex_digest(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
fn sha_digest(value: &str) -> bool {
    value
        .strip_prefix("sha256:")
        .is_some_and(|value| hex_digest(value, 64))
}

#[cfg(test)]
#[path = "fork_release_auth_tests.rs"]
mod tests;

impl AuthenticatedForkRelease {
    pub(crate) fn archive_bytes(&self) -> u64 {
        self.0.archive_bytes
    }
    pub(crate) fn archive_sha256(&self) -> &str {
        &self.0.archive_sha256
    }
}
