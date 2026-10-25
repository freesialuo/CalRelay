//! Bounded remote fetch contract; no database mutation or protocol-to-event mapping.
use std::{future::Future, time::Duration};

#[derive(Clone, Default)]
pub struct RemoteValidators {
    pub etag: Option<String>,
    pub last_modified: Option<String>,
}
#[derive(Clone)]
pub enum RemoteAuthentication {
    None,
    Basic { credential_id: String },
    Bearer { credential_id: String },
}
pub struct RemoteFetchRequest {
    pub url: String,
    pub authentication: RemoteAuthentication,
    pub validators: RemoteValidators,
    /// Trusted caller sets this only for a successfully committed snapshot.
    pub has_committed_snapshot: bool,
    pub now_ms: i64,
}
pub struct RemoteDocument {
    pub body: Vec<u8>,
    pub validators: RemoteValidators,
}
pub enum RemoteFetchOutcome {
    Modified(RemoteDocument),
    NotModified(RemoteValidators),
}
#[derive(Debug, Clone, Copy)]
pub struct RemoteFetchLimits {
    pub max_bytes: usize,
    pub max_redirects: usize,
    /// Covers credentials, DNS, every redirect and the complete streamed body.
    pub timeout: Duration,
}
impl Default for RemoteFetchLimits {
    fn default() -> Self {
        Self {
            max_bytes: 8 * 1024 * 1024,
            max_redirects: 5,
            timeout: Duration::from_secs(30),
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum RemoteFetchError {
    #[error("invalid remote configuration")]
    InvalidConfiguration,
    #[error("remote address denied")]
    AddressDenied,
    #[error("remote DNS resolution failed")]
    Dns,
    #[error("remote fetch deadline exceeded")]
    Timeout,
    #[error("remote transport failed")]
    Transport,
    #[error("remote response exceeds byte budget")]
    TooLarge,
    #[error("remote redirect rejected")]
    Redirect,
    #[error("remote credential unavailable or invalid")]
    Credential,
    #[error("remote authentication rejected")]
    Unauthorized,
    #[error("remote rate limited")]
    RateLimited { retry_after_ms: Option<i64> },
    #[error("remote server failed")]
    Server {
        status: u16,
        retry_after_ms: Option<i64>,
    },
    #[error("remote HTTP status rejected")]
    Status { status: u16 },
    #[error("304 requires a committed snapshot")]
    UnexpectedNotModified,
    #[error("remote response protocol invalid")]
    Protocol,
}
pub trait RemoteFetcher: Send + Sync {
    fn fetch(
        &self,
        request: RemoteFetchRequest,
    ) -> impl Future<Output = Result<RemoteFetchOutcome, RemoteFetchError>> + Send;
}
