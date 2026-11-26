use std::time::Duration;

use hdrhistogram::Histogram;
use tracing::info;

// One worker owns four histograms. Bound their memory use even when a target
// is stalled for a long time; values beyond one day are recorded at the cap.
const MAX_RECORDED_MICROS: u64 = 86_400_000_000;

pub struct LatencyMetrics {
    semaphore_wait: Histogram<u64>,
    request_gate_wait: Histogram<u64>,
    observation: Histogram<u64>,
    scheduled_to_completion: Histogram<u64>,
}

impl LatencyMetrics {
    fn histogram() -> Result<Histogram<u64>, hdrhistogram::CreationError> {
        Histogram::new_with_bounds(1, MAX_RECORDED_MICROS, 2)
    }

    pub fn new() -> Result<Self, hdrhistogram::CreationError> {
        Ok(Self {
            semaphore_wait: Self::histogram()?,
            request_gate_wait: Self::histogram()?,
            observation: Self::histogram()?,
            scheduled_to_completion: Self::histogram()?,
        })
    }

    pub fn record(&mut self, semaphore_wait: Duration, gate_wait: Duration,
                  observation: Duration, total: Duration) {
        fn micros(duration: Duration) -> u64 {
            duration.as_micros().max(1).min(MAX_RECORDED_MICROS as u128) as u64
        }
        let _ = self.semaphore_wait.record(micros(semaphore_wait));
        let _ = self.request_gate_wait.record(micros(gate_wait));
        let _ = self.observation.record(micros(observation));
        let _ = self.scheduled_to_completion.record(micros(total));
    }

    /// Merge one worker's private histograms into the final process summary.
    /// Each worker records locally, avoiding a contended shared mutex on the
    /// request hot path.
    pub fn merge(&mut self, other: Self) {
        self.semaphore_wait.add(&other.semaphore_wait)
            .expect("worker histogram configuration must match");
        self.request_gate_wait.add(&other.request_gate_wait)
            .expect("worker histogram configuration must match");
        self.observation.add(&other.observation)
            .expect("worker histogram configuration must match");
        self.scheduled_to_completion.add(&other.scheduled_to_completion)
            .expect("worker histogram configuration must match");
    }

    pub fn log_summary(&self) {
        for (name, histogram) in [
            ("semaphore_wait", &self.semaphore_wait),
            ("request_gate_wait", &self.request_gate_wait),
            ("observation", &self.observation),
            ("scheduled_to_completion", &self.scheduled_to_completion),
        ] {
            if histogram.len() > 0 {
                info!(
                    metric = name,
                    samples = histogram.len(),
                    p50_us = histogram.value_at_quantile(0.50),
                    p95_us = histogram.value_at_quantile(0.95),
                    p99_us = histogram.value_at_quantile(0.99),
                    max_us = histogram.max(),
                    "latency metric summary"
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_all_metric_samples() {
        let mut metrics = LatencyMetrics::new().unwrap();
        metrics.record(
            Duration::from_micros(10),
            Duration::from_micros(20),
            Duration::from_micros(30),
            Duration::from_micros(60),
        );
        assert_eq!(metrics.semaphore_wait.len(), 1);
        assert_eq!(metrics.request_gate_wait.len(), 1);
        assert_eq!(metrics.observation.len(), 1);
        assert_eq!(metrics.scheduled_to_completion.len(), 1);
    }

    #[test]
    fn merges_worker_histograms_without_losing_samples() {
        let mut aggregate = LatencyMetrics::new().unwrap();
        let mut worker = LatencyMetrics::new().unwrap();
        worker.record(
            Duration::from_micros(11),
            Duration::from_micros(21),
            Duration::from_micros(31),
            Duration::from_micros(63),
        );
        aggregate.merge(worker);
        assert_eq!(aggregate.semaphore_wait.len(), 1);
        assert_eq!(aggregate.request_gate_wait.len(), 1);
        assert_eq!(aggregate.observation.len(), 1);
        assert_eq!(aggregate.scheduled_to_completion.len(), 1);
    }

    #[test]
    fn clamps_extreme_samples_to_bounded_histogram_range() {
        let mut metrics = LatencyMetrics::new().unwrap();
        metrics.record(
            Duration::from_secs(90_000),
            Duration::ZERO,
            Duration::ZERO,
            Duration::from_secs(90_000),
        );
        assert_eq!(metrics.semaphore_wait.max(), MAX_RECORDED_MICROS);
        assert_eq!(metrics.scheduled_to_completion.max(), MAX_RECORDED_MICROS);
    }
}
