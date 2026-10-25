//! Secret values cross application boundaries explicitly and never implement Display/Serialize.
use std::{fmt, future::Future};
use zeroize::Zeroizing;

pub struct SecretBytes(Zeroizing<Vec<u8>>, Option<u64>);
impl SecretBytes {
    pub fn new(bytes: Vec<u8>) -> Self {
        Self(Zeroizing::new(bytes), None)
    }
    /// Provider revision from the same record read as this secret.
    pub fn with_revision(mut self, revision: u64) -> Self {
        self.1 = Some(revision);
        self
    }
    pub fn revision(&self) -> Option<u64> {
        self.1
    }
    /// Only expose at a trusted cryptographic or transport boundary.
    pub fn expose(&self) -> &[u8] {
        &self.0
    }
}
impl fmt::Debug for SecretBytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretBytes([REDACTED])")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SecurityError {
    #[error("invalid security input")]
    InvalidInput,
    #[error("master key file unavailable or invalid")]
    KeyFile,
    #[error("required master key version unavailable")]
    MissingKey,
    #[error("credential authentication failed")]
    Authentication,
    #[error("security record not found or access denied")]
    Denied,
    #[error("security record revision conflict")]
    Conflict,
    #[error("secure randomness unavailable")]
    Randomness,
    #[error("security storage operation failed")]
    Storage,
}

pub trait CredentialProvider: Send + Sync {
    fn credential(
        &self,
        id: &str,
        expected_kind: &str,
    ) -> impl Future<Output = Result<SecretBytes, SecurityError>> + Send;
}
