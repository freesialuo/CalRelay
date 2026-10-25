//! Business delivery driver. Production composition supplies a configured output and clock.
use crate::Services;
use calendar_application::{deliver_one, Clock, DeliveryError, PushOutput};
use std::time::Duration;

#[derive(Clone, Copy, Debug)]
pub struct DeliveryWorkerLimits {
    max_operations: usize,
    round_budget_ms: u64,
    scan_delay: Duration,
}
impl DeliveryWorkerLimits {
    /// Resource budgets only; these do not configure rules, outputs or source freshness.
    pub fn new(
        max_operations: usize,
        round_budget: Duration,
        scan_delay: Duration,
    ) -> Result<Self, DeliveryError> {
        if !(1..=256).contains(&max_operations)
            || round_budget < Duration::from_millis(1)
            || round_budget > Duration::from_secs(120)
            || scan_delay < Duration::from_millis(1)
            || scan_delay > Duration::from_secs(60)
        {
            return Err(DeliveryError::InvalidPolicy);
        }
        Ok(Self {
            max_operations,
            round_budget_ms: round_budget.as_millis() as u64,
            scan_delay,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeliveryProgress {
    /// A leased job completed prepare/finish, or was invalidated before sending.
    /// This is not evidence of remote acceptance or device display.
    Processed,
    /// The real repository query found no claimable job.
    Empty,
    /// A real read observed the persistent restore pause; no claim was attempted.
    RecoveryPaused,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeliveryRoundEnd {
    Empty,
    RecoveryPaused,
    OperationLimit,
    Deadline,
    Stopped,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeliveryRound {
    pub processed: usize,
    pub end: DeliveryRoundEnd,
}

/// Admit each bounded operation separately, so shutdown can drain its final transaction.
/// The monotonic budget limits *new admissions*: it never drops an in-flight send/finish.
/// The output's request timeout and the enclosing supervisor's finite drain still apply.
pub async fn delivery_round<O: PushOutput, C: Clock + ?Sized>(
    services: &Services,
    output: &O,
    owner: &str,
    limits: DeliveryWorkerLimits,
    clock: &C,
    mut progress: impl FnMut(DeliveryProgress),
) -> Result<DeliveryRound, DeliveryError> {
    services.delivery_policy.validate()?;
    if owner.is_empty() || owner.len() > 128 {
        return Err(DeliveryError::InvalidPolicy);
    }
    let started = clock.monotonic_now_ms();
    let mut processed = 0;
    let end = loop {
        if !services.ready() {
            break DeliveryRoundEnd::Stopped;
        }
        if processed >= limits.max_operations {
            break DeliveryRoundEnd::OperationLimit;
        }
        if clock.monotonic_now_ms().saturating_sub(started) >= limits.round_budget_ms {
            break DeliveryRoundEnd::Deadline;
        }
        let result = services
            .run_task(async |repository| {
                if repository.delivery_paused().await? {
                    return Ok(DeliveryProgress::RecoveryPaused);
                }
                deliver_one(repository, output, owner, &services.delivery_policy, clock)
                    .await
                    .map(|processed| {
                        if processed {
                            DeliveryProgress::Processed
                        } else {
                            DeliveryProgress::Empty
                        }
                    })
            })
            .await;
        match result {
            None => break DeliveryRoundEnd::Stopped,
            Some(Err(error)) => return Err(error),
            Some(Ok(DeliveryProgress::RecoveryPaused)) => {
                progress(DeliveryProgress::RecoveryPaused);
                break DeliveryRoundEnd::RecoveryPaused;
            }
            Some(Ok(DeliveryProgress::Empty)) => {
                progress(DeliveryProgress::Empty);
                break DeliveryRoundEnd::Empty;
            }
            Some(Ok(DeliveryProgress::Processed)) => {
                processed += 1;
                progress(DeliveryProgress::Processed);
            }
        }
    };
    Ok(DeliveryRound { processed, end })
}

/// Run real queue rounds, with a cancellable delay after every round (even a full one).
/// Storage errors return to supervision; external failures remain durable retry outcomes.
/// Waiting produces no progress reports. No worker is installed by the standard binary yet.
/// `output` must dispatch every claimed envelope's `output_id`; claims are not output-filtered.
pub async fn run_delivery_worker<O: PushOutput, C: Clock + ?Sized>(
    services: &Services,
    output: &O,
    owner: &str,
    limits: DeliveryWorkerLimits,
    clock: &C,
    mut progress: impl FnMut(DeliveryProgress),
) -> Result<(), DeliveryError> {
    let mut shutdown = services.shutdown.subscribe();
    loop {
        let round = delivery_round(services, output, owner, limits, clock, &mut progress).await?;
        if round.end == DeliveryRoundEnd::Stopped || *shutdown.borrow_and_update() {
            return Ok(());
        }
        tokio::select! {
            () = tokio::time::sleep(limits.scan_delay) => {},
            _ = shutdown.changed() => {},
        }
    }
}
