//! Bounded delivery use case. Channel adapters return classified, credential-free outcomes.
use crate::{Clock, EventV1};
use std::{future::Future, time::Duration};

#[derive(Debug, Clone)]
pub struct DeliveryPolicy {
    pub lease_ms: i64,
    pub request_timeout_ms: i64,
    pub catch_up_ms: i64,
    pub max_attempts: i64,
    pub base_backoff_ms: i64,
    pub max_backoff_ms: i64,
    pub source_max_age_ms: i64,
    pub tzdata_version: String,
}
impl Default for DeliveryPolicy {
    fn default() -> Self {
        Self {
            lease_ms: 60_000,
            request_timeout_ms: 30_000,
            catch_up_ms: 900_000,
            max_attempts: 8,
            base_backoff_ms: 1_000,
            max_backoff_ms: 60_000,
            source_max_age_ms: 86_400_000,
            tzdata_version: "2025b".into(),
        }
    }
}
impl DeliveryPolicy {
    pub fn validate(&self) -> Result<(), DeliveryError> {
        if self.request_timeout_ms <= 0
            || self.lease_ms <= self.request_timeout_ms
            || self.catch_up_ms <= 0
            || self.max_attempts < 1
            || self.base_backoff_ms <= 0
            || self.max_backoff_ms < self.base_backoff_ms
            || self.source_max_age_ms <= 0
            || self.tzdata_version.is_empty()
        {
            return Err(DeliveryError::InvalidPolicy);
        }
        Ok(())
    }
    /// Equal jitter, bounded exponential delay. The caller supplies stable per-attempt entropy.
    pub fn retry_delay_ms(&self, attempt: i64, entropy: u64) -> i64 {
        let factor = 1_i64
            .checked_shl(attempt.saturating_sub(1).clamp(0, 62) as u32)
            .unwrap_or(i64::MAX);
        let cap = self
            .base_backoff_ms
            .saturating_mul(factor)
            .min(self.max_backoff_ms)
            .max(1);
        cap / 2 + (entropy % (cap - cap / 2 + 1) as u64) as i64
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DeliveryError {
    #[error("invalid delivery policy or worker identity")]
    InvalidPolicy,
    #[error("lease expired or no longer owned")]
    StaleLease,
    #[error("SQLite is busy; retry on a later scan")]
    Busy,
    // Never expose arbitrary database/adapter error strings containing credentials.
    #[error("delivery storage operation failed")]
    Database,
    #[error("persisted delivery data is invalid")]
    InvalidData,
}

#[derive(Clone, PartialEq, Eq)]
pub struct DeliveryLease {
    pub job_id: String,
    pub owner: String,
    pub token: String,
}
// A lease token is a capability; omit it from diagnostics.
impl std::fmt::Debug for DeliveryLease {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeliveryLease")
            .field("job_id", &self.job_id)
            .finish_non_exhaustive()
    }
}

pub struct DeliveryEnvelope {
    pub job_id: String,
    pub request_id: String,
    pub output_id: String,
    pub event: EventV1,
    pub original_key: String,
    pub due_at: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryOutcome {
    Accepted,
    PermanentFailure,
    Retryable {
        retry_after_ms: Option<i64>,
    },
    /// The request may have been accepted remotely (including a local timeout).
    Unknown {
        retry_after_ms: Option<i64>,
    },
}

pub trait DeliveryRepository: Sync {
    fn claim<C: Clock + ?Sized>(
        &self,
        owner: &str,
        policy: &DeliveryPolicy,
        clock: &C,
    ) -> impl Future<Output = Result<Option<DeliveryLease>, DeliveryError>> + Send;
    /// Final validity check and durable send intent; called immediately before external I/O.
    fn prepare<C: Clock + ?Sized>(
        &self,
        lease: &DeliveryLease,
        policy: &DeliveryPolicy,
        clock: &C,
    ) -> impl Future<Output = Result<Option<DeliveryEnvelope>, DeliveryError>> + Send;
    fn finish<C: Clock + ?Sized>(
        &self,
        lease: &DeliveryLease,
        outcome: DeliveryOutcome,
        policy: &DeliveryPolicy,
        clock: &C,
    ) -> impl Future<Output = Result<(), DeliveryError>> + Send;
}

pub trait PushOutput: Sync {
    fn deliver(&self, envelope: DeliveryEnvelope) -> impl Future<Output = DeliveryOutcome> + Send;
}

/// One bounded scan/attempt; the server owns wakeups and stops calling this during shutdown.
/// Requires a Tokio runtime with timers enabled.
/// Adapters must yield and be cancellation-safe; timeout cannot preempt blocking code.
pub async fn deliver_one<R: DeliveryRepository, O: PushOutput, C: Clock + ?Sized>(
    repository: &R,
    output: &O,
    owner: &str,
    policy: &DeliveryPolicy,
    clock: &C,
) -> Result<bool, DeliveryError> {
    let Some(lease) = repository.claim(owner, policy, clock).await? else {
        return Ok(false);
    };
    let Some(envelope) = repository.prepare(&lease, policy, clock).await? else {
        return Ok(true);
    };
    let outcome = tokio::time::timeout(
        Duration::from_millis(policy.request_timeout_ms as u64),
        output.deliver(envelope),
    )
    .await
    .unwrap_or(DeliveryOutcome::Unknown {
        retry_after_ms: None,
    });
    repository.finish(&lease, outcome, policy, clock).await?;
    Ok(true)
}
