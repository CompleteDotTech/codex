//! Host-side lookup of candidate credential references. Lookup never selects a backend.

use crate::profile::CredentialSource;
use codex_keyring_store::KeyringStore;
use std::env;
use std::fmt;
use zeroize::Zeroizing;

/// The keyring service that holds every credential a storage profile refers to.
pub const KEYRING_SERVICE: &str = "codex-postgres-storage";
const MAX_CREDENTIAL_BYTES: usize = 8192;

/// A credential held only for a connection attempt and zeroed on drop.
pub struct ResolvedCredential(Zeroizing<String>);

impl ResolvedCredential {
    /// Expose the password only to a trusted connection builder.
    pub fn expose(&self) -> &str {
        self.0.as_str()
    }

    /// Move the secret into another zeroizing owner without cloning plaintext.
    pub fn into_zeroizing(self) -> Zeroizing<String> {
        self.0
    }
}

impl fmt::Debug for ResolvedCredential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ResolvedCredential([redacted])")
    }
}

/// A host-owned resolver. Call it only after trusted-layer candidate selection;
/// never from a project config parser, client process, or model tool call.
pub struct HostCredentialResolver<'a> {
    keyring: &'a dyn KeyringStore,
}

impl<'a> HostCredentialResolver<'a> {
    pub fn new(keyring: &'a dyn KeyringStore) -> Self {
        Self { keyring }
    }

    /// Resolve afresh per connection attempt so credential rotation takes effect.
    /// The keyring account is the candidate's opaque ID in the dedicated service.
    pub fn resolve(
        &self,
        source: &CredentialSource,
    ) -> Result<ResolvedCredential, CredentialResolutionError> {
        self.resolve_with_environment(source, |name| match env::var(name) {
            Ok(value) => Ok(Some(value)),
            Err(env::VarError::NotPresent) => Ok(None),
            Err(env::VarError::NotUnicode(_)) => Err(CredentialResolutionError::InvalidValue),
        })
    }

    fn resolve_with_environment(
        &self,
        source: &CredentialSource,
        environment: impl FnOnce(&str) -> Result<Option<String>, CredentialResolutionError>,
    ) -> Result<ResolvedCredential, CredentialResolutionError> {
        let value = match source {
            CredentialSource::Environment { variable } => environment(variable.as_str())?,
            CredentialSource::Keyring { id } => self
                .keyring
                .load(KEYRING_SERVICE, id.as_str())
                .map_err(|_| CredentialResolutionError::StoreUnavailable)?,
        };
        let value = Zeroizing::new(value.ok_or(CredentialResolutionError::Missing)?);
        if value.is_empty() || value.len() > MAX_CREDENTIAL_BYTES {
            return Err(CredentialResolutionError::InvalidValue);
        }
        Ok(ResolvedCredential(value))
    }
}

/// Redacted lookup result; neither source names nor secret values enter errors.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CredentialResolutionError {
    Missing,
    InvalidValue,
    StoreUnavailable,
}

impl fmt::Display for CredentialResolutionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::Missing => "storage credential is missing",
            Self::InvalidValue => "storage credential has an invalid size",
            Self::StoreUnavailable => "storage credential store is unavailable",
        };
        f.write_str(message)
    }
}

impl std::error::Error for CredentialResolutionError {}

#[cfg(test)]
#[path = "credential_resolution_tests.rs"]
mod tests;
