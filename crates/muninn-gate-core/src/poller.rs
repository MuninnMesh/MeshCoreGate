//! Poll scheduling for configured telemetry producers.
//!
//! The scheduler is intentionally small and deterministic:
//!
//! - At boot, enabled producers are spread across the configured default interval. With a 10 minute
//!   interval and three producers, the initial due times are roughly 0, 3.3, and 6.6 minutes apart,
//!   plus deterministic per-producer jitter. This avoids a burst of LoRa airtime at startup.
//! - Each producer may override the default interval in configuration. After a successful poll, the
//!   next due time is based on that effective interval.
//! - Failures are retried only up to `PollingConfig::retry_count`. Retry delay is the configured
//!   jitter window, with a one second minimum so failed polls never spin tightly.
//! - `force_all_due` and `force_due` are the only on-demand controls. Platform code can call them
//!   from USB, HTTP, or a button without changing scheduler policy.
//!
//! This module does not own a timer, async runtime, radio queue, or telemetry
//! storage implementation. It only decides which producers are due and updates
//! the supplied `PollTelemetryStore` after the supplied `MeshcoreClient`
//! returns.

use heapless::Vec;

use crate::{Error, GatewayConfig, MeshcoreClient, PollTelemetryStore, TelemetryProducerId};

/// Scheduled poll state for one telemetry producer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScheduledProducer
{
    /// Producer identifier due for polling.
    pub producer_id:    TelemetryProducerId,
    /// Monotonic timestamp when the producer should next be polled.
    pub next_poll_ms:   u64,
    /// Retry attempts already used for the current scheduled poll.
    pub retry_attempts: u8,
}

/// Fixed-capacity scheduler for telemetry producer polling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PollScheduler<const N: usize>
{
    entries: Vec<ScheduledProducer, N>,
}

impl<const N: usize> PollScheduler<N>
{
    /// Create a scheduler from enabled producers in gateway config.
    ///
    /// Initial poll times are spread across the default polling interval so a
    /// gateway with multiple producers does not burst all requests at boot.
    pub fn new(config: &GatewayConfig<N>, start_ms: u64) -> Result<Self, Error>
    {
        let mut scheduler = Self {
            entries: Vec::new(),
        };
        let enabled_count = config.enabled_producers().count() as u64;
        let default_interval_ms = config.polling.default_interval_ms();
        let spacing_ms = if enabled_count > 1 {
            default_interval_ms / enabled_count
        } else {
            0
        };

        for (index, producer) in config.enabled_producers().enumerate() {
            let producer_id = producer.id();
            let offset_ms = (index as u64)
                .saturating_mul(spacing_ms)
                .saturating_add(producer_jitter_ms(producer_id, config.polling.jitter_ms()));
            scheduler
                .entries
                .push(ScheduledProducer {
                    producer_id,
                    next_poll_ms: start_ms.saturating_add(offset_ms),
                    retry_attempts: 0,
                })
                .map_err(|_| Error::Capacity)?;
        }

        Ok(scheduler)
    }

    /// Return producer IDs due at `now_ms`.
    pub fn due_producers(&self, now_ms: u64) -> Vec<TelemetryProducerId, N>
    {
        let mut due = Vec::new();
        for entry in self.entries.iter() {
            if entry.next_poll_ms <= now_ms {
                let _ = due.push(entry.producer_id);
            }
        }
        due
    }

    /// Force every configured producer to be due immediately.
    pub fn force_all_due(&mut self, now_ms: u64)
    {
        for entry in self.entries.iter_mut() {
            entry.next_poll_ms = now_ms;
            entry.retry_attempts = 0;
        }
    }

    /// Force one configured producer to be due immediately.
    pub fn force_due(&mut self, producer_id: TelemetryProducerId, now_ms: u64)
    -> Result<(), Error>
    {
        let entry = self
            .entries
            .iter_mut()
            .find(|entry| entry.producer_id == producer_id)
            .ok_or(Error::ProducerNotFound)?;
        entry.next_poll_ms = now_ms;
        entry.retry_attempts = 0;
        Ok(())
    }

    /// Mark one producer polled and schedule the next poll.
    pub fn mark_polled(
        &mut self,
        producer_id: TelemetryProducerId,
        now_ms: u64,
        interval_ms: u64,
    ) -> Result<(), Error>
    {
        let entry = self
            .entries
            .iter_mut()
            .find(|entry| entry.producer_id == producer_id)
            .ok_or(Error::ProducerNotFound)?;
        entry.next_poll_ms = now_ms.saturating_add(interval_ms);
        entry.retry_attempts = 0;
        Ok(())
    }

    /// Mark one producer for a later retry of the same scheduled poll.
    pub fn mark_retry(
        &mut self,
        producer_id: TelemetryProducerId,
        now_ms: u64,
        retry_delay_ms: u64,
    ) -> Result<(), Error>
    {
        let entry = self
            .entries
            .iter_mut()
            .find(|entry| entry.producer_id == producer_id)
            .ok_or(Error::ProducerNotFound)?;
        entry.retry_attempts = entry.retry_attempts.saturating_add(1);
        entry.next_poll_ms = now_ms.saturating_add(retry_delay_ms);
        Ok(())
    }

    /// Return the next scheduled wake timestamp.
    pub fn next_wake_ms(&self) -> Option<u64>
    {
        self.entries.iter().map(|entry| entry.next_poll_ms).min()
    }

    /// Poll all producers due at `now_ms`.
    pub async fn poll_due_producers<C, S>(
        &mut self,
        config: &GatewayConfig<N>,
        client: &mut C,
        store: &mut S,
        now_ms: u64,
    ) -> PollSummary
    where
        C: MeshcoreClient,
        S: PollTelemetryStore<N>,
    {
        let mut summary = PollSummary::default();
        let due = self.due_producers(now_ms);

        for producer_id in due {
            let Some(producer) = config.producer(producer_id) else {
                continue;
            };

            summary.attempted = summary.attempted.saturating_add(1);
            match client.poll_producer(producer).await {
                Ok(mut telemetry) => {
                    telemetry.producer_id = producer_id;
                    if telemetry.timestamp_ms == 0 {
                        telemetry.timestamp_ms = now_ms;
                    }

                    if store.update_producer(producer_id, telemetry).is_ok()
                        && store.record_poll_success(producer_id, now_ms).is_ok()
                    {
                        summary.succeeded = summary.succeeded.saturating_add(1);
                    } else {
                        let _ = store.record_poll_failure(producer_id, now_ms);
                        summary.failed = summary.failed.saturating_add(1);
                    }
                    let interval_ms = producer.effective_polling_interval_ms(config.polling);
                    let _ = self.mark_polled(producer_id, now_ms, interval_ms);
                },
                Err(Error::Unsupported) => {
                    let _ = store.record_poll_failure(producer_id, now_ms);
                    summary.failed = summary.failed.saturating_add(1);
                    let interval_ms = producer.effective_polling_interval_ms(config.polling);
                    let _ = self.mark_polled(producer_id, now_ms, interval_ms);
                },
                Err(_) if self.retry_attempts(producer_id) < config.polling.retry_count => {
                    let _ = self.mark_retry(producer_id, now_ms, retry_delay_ms(config));
                    summary.retrying = summary.retrying.saturating_add(1);
                },
                Err(_) => {
                    let _ = store.record_poll_failure(producer_id, now_ms);
                    summary.failed = summary.failed.saturating_add(1);
                    let interval_ms = producer.effective_polling_interval_ms(config.polling);
                    let _ = self.mark_polled(producer_id, now_ms, interval_ms);
                },
            }
        }

        summary
    }

    fn retry_attempts(&self, producer_id: TelemetryProducerId) -> u8
    {
        self.entries
            .iter()
            .find(|entry| entry.producer_id == producer_id)
            .map(|entry| entry.retry_attempts)
            .unwrap_or(0)
    }
}

/// Summary of a scheduler polling pass.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct PollSummary
{
    /// Number of producers attempted.
    pub attempted: u32,
    /// Number of producers successfully polled.
    pub succeeded: u32,
    /// Number of producers that failed.
    pub failed:    u32,
    /// Number of producers scheduled for a retry.
    pub retrying:  u32,
}

fn producer_jitter_ms(producer_id: TelemetryProducerId, jitter_ms: u64) -> u64
{
    if jitter_ms == 0 {
        return 0;
    }

    let mut value = producer_id.as_u64();
    value ^= value >> 33;
    value = value.wrapping_mul(0xff51afd7ed558ccd);
    value ^= value >> 33;
    value = value.wrapping_mul(0xc4ceb9fe1a85ec53);
    value ^= value >> 33;
    value % (jitter_ms + 1)
}

fn retry_delay_ms<const N: usize>(config: &GatewayConfig<N>) -> u64
{
    config.polling.jitter_ms().max(1_000)
}

#[cfg(test)]
mod tests
{
    use core::future::{Ready, ready};
    use core::pin::pin;

    use super::PollScheduler;
    use crate::config::{GatewayConfig, TelemetryProducerConfig, fixed_string};
    use crate::{
        Error,
        FixedTelemetryStore,
        MeshcoreClient,
        ProducerTelemetry,
        TelemetryProducerConfig as ProducerConfig,
        TelemetryStore,
    };

    #[test]
    fn staggers_initial_polls_across_default_interval()
    {
        let mut config = GatewayConfig::<4>::new("gate").unwrap();
        config.meshcore.public_key = Some(fixed_string("gateway-public-key").unwrap());
        config.polling.default_interval_secs = 600;
        config.polling.jitter_secs = 0;
        for id in 1..=3 {
            config
                .add_producer(TelemetryProducerConfig::new(producer_key(id), "producer").unwrap())
                .unwrap();
        }

        let scheduler = PollScheduler::new(&config, 1_000).unwrap();

        assert_eq!(scheduler.entries[0].next_poll_ms, 1_000);
        assert_eq!(scheduler.entries[1].next_poll_ms, 201_000);
        assert_eq!(scheduler.entries[2].next_poll_ms, 401_000);
    }

    #[test]
    fn clamps_initial_spacing_interval_through_polling_policy()
    {
        let mut config = GatewayConfig::<2>::new("gate").unwrap();
        config.meshcore.public_key = Some(fixed_string("gateway-public-key").unwrap());
        config.polling.default_interval_secs = 1;
        config.polling.min_interval_secs = 60;
        config.polling.jitter_secs = 0;
        for id in 1..=2 {
            config
                .add_producer(TelemetryProducerConfig::new(producer_key(id), "producer").unwrap())
                .unwrap();
        }

        let scheduler = PollScheduler::new(&config, 0).unwrap();

        assert_eq!(scheduler.entries[0].next_poll_ms, 0);
        assert_eq!(scheduler.entries[1].next_poll_ms, 30_000);
    }

    #[test]
    fn unsupported_active_poll_fails_without_retrying()
    {
        let mut config = GatewayConfig::<1>::new("gate").unwrap();
        config.meshcore.public_key = Some(fixed_string("gateway-public-key").unwrap());
        config.polling.default_interval_secs = 600;
        config.polling.jitter_secs = 0;
        config.polling.retry_count = 5;
        let producer = TelemetryProducerConfig::new(producer_key(1), "producer").unwrap();
        let producer_id = producer.id();
        config.add_producer(producer).unwrap();
        let mut scheduler = PollScheduler::new(&config, 0).unwrap();
        let mut store = FixedTelemetryStore::from_config(&config).unwrap();
        let mut client = UnsupportedClient;

        let summary = block_on(scheduler.poll_due_producers(&config, &mut client, &mut store, 0));

        assert_eq!(summary.attempted, 1);
        assert_eq!(summary.succeeded, 0);
        assert_eq!(summary.failed, 1);
        assert_eq!(summary.retrying, 0);
        assert_eq!(scheduler.entries[0].retry_attempts, 0);
        assert_eq!(scheduler.entries[0].next_poll_ms, 600_000);

        let snapshot = store.snapshot();
        let record = snapshot
            .records
            .iter()
            .find(|record| record.config.id() == producer_id)
            .unwrap();
        assert_eq!(record.poll.poll_failure_total, 1);
        assert_eq!(record.poll.last_poll_success, Some(false));
        assert_eq!(snapshot.gateway.poll_failure_total, 1);
    }

    fn producer_key(id: u64) -> &'static str
    {
        match id {
            1 => "producer-public-key-1",
            2 => "producer-public-key-2",
            _ => "producer-public-key-3",
        }
    }

    struct UnsupportedClient;

    impl MeshcoreClient for UnsupportedClient
    {
        type PollFuture<'a> = Ready<Result<ProducerTelemetry, Error>>;

        fn poll_producer<'a>(&'a mut self, _producer: &'a ProducerConfig) -> Self::PollFuture<'a>
        {
            ready(Err(Error::Unsupported))
        }
    }

    fn block_on<F>(future: F) -> F::Output
    where
        F: core::future::Future,
    {
        use std::sync::Arc;
        use std::task::{Context, Poll, Wake, Waker};

        struct NoopWake;

        impl Wake for NoopWake
        {
            fn wake(self: Arc<Self>) {}
        }

        let waker = Waker::from(Arc::new(NoopWake));
        let mut context = Context::from_waker(&waker);
        let mut future = pin!(future);
        loop {
            match future.as_mut().poll(&mut context) {
                Poll::Ready(output) => return output,
                Poll::Pending => std::thread::yield_now(),
            }
        }
    }
}
