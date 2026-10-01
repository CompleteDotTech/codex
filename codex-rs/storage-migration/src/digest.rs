//! Order-defined digests that let two stores prove they hold the same records.

use serde::Serialize;
use sha2::Digest;
use sha2::Sha256;

/// Count and digest of one domain. Records are hashed as canonical JSON lines in key order, so
/// two stores holding the same records in the same key order produce the same digest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DomainDigest {
    pub count: u64,
    pub digest: String,
}

pub(crate) struct DigestBuilder {
    hasher: Sha256,
    count: u64,
}

impl DigestBuilder {
    pub(crate) fn new() -> Self {
        Self {
            hasher: Sha256::new(),
            count: 0,
        }
    }

    pub(crate) fn add<R: Serialize>(&mut self, record: &R) -> serde_json::Result<()> {
        let line = serde_json::to_string(record)?;
        self.hasher.update((line.len() as u64).to_be_bytes());
        self.hasher.update(line.as_bytes());
        self.count += 1;
        Ok(())
    }

    pub(crate) fn finish(self) -> DomainDigest {
        let bytes = self.hasher.finalize();
        let digest = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
        DomainDigest {
            count: self.count,
            digest,
        }
    }
}
