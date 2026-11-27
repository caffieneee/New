use std::time::Duration;

use hdrhistogram::Histogram;
use tracing::info;

// Bound memory use even when a target is stalled for a long time; values beyond
// one day are recorded at the cap.
const MAX_RECORDED_MICROS: u64 = 86_400_000_000;

pub struct LatencyMetrics {
    dispatch_wait: Histogram<u64>,
    observation: Histogram<u64>,
    scheduled_to_completion: Histogram<u64>,
}

impl LatencyMetrics {
    fn histogram() -> Result<Histogram<u64>, hdrhistogram::CreationError> {
        Histogram::new_with_bounds(1, MAX_RECORDED_MICROS, 2)
    }

    pub fn new() -> Result<Self, hdrhistogram::CreationError> {
        Ok(Self {
            dispatch_wait: Self::histogram()?,
            observation: Self::histogram()?,
            scheduled_to_completion: Self::histogram()?,
        })
    }

    pub fn record(&mut self, dispatch_wait: Duration, observation: Duration, total: Duration) {
        fn micros(duration: Duration) -> u64 {
            duration.as_micros().max(1).min(MAX_RECORDED_MICROS as u128) as u64
        }
        let _ = self.dispatch_wait.record(micros(dispatch_wait));
        let _ = self.observation.record(micros(observation));
        let _ = self.scheduled_to_completion.record(micros(total));
    }

    pub fn merge(&mut self, other: Self) {
        self.dispatch_wait.add(&other.dispatch_wait)
            .expect("target histogram configuration must match");
        self.observation.add(&other.observation)
            .expect("target histogram configuration must match");
        self.scheduled_to_completion.add(&other.scheduled_to_completion)
            .expect("target histogram configuration must match");
    }

    pub fn log_summary(&self) {
        self.log_summary_for("aggregate", false);
    }

    pub fn log_summary_for(&self, target: &str, priority: bool) {
        for (name, histogram) in [
            ("dispatch_wait", &self.dispatch_wait),
            ("observation", &self.observation),
            ("scheduled_to_completion", &self.scheduled_to_completion),
        ] {
            if histogram.len() > 0 {
                info!(
                    target,
                    priority,
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
            Duration::from_micros(20),
            Duration::from_micros(30),
            Duration::from_micros(60),
        );
        assert_eq!(metrics.dispatch_wait.len(), 1);
        assert_eq!(metrics.observation.len(), 1);
        assert_eq!(metrics.scheduled_to_completion.len(), 1);
    }

    #[test]
    fn merges_target_histograms_without_losing_samples() {
        let mut aggregate = LatencyMetrics::new().unwrap();
        let mut target = LatencyMetrics::new().unwrap();
        target.record(
            Duration::from_micros(21),
            Duration::from_micros(31),
            Duration::from_micros(63),
        );
        aggregate.merge(target);
        assert_eq!(aggregate.dispatch_wait.len(), 1);
        assert_eq!(aggregate.observation.len(), 1);
        assert_eq!(aggregate.scheduled_to_completion.len(), 1);
    }

    #[test]
    fn clamps_extreme_samples_to_bounded_histogram_range() {
        let mut metrics = LatencyMetrics::new().unwrap();
        metrics.record(
            Duration::from_secs(90_000),
            Duration::ZERO,
            Duration::from_secs(90_000),
        );

        // HDR histograms report the upper edge of a quantized bucket, which
        // can be slightly above the exact recorded cap at two significant figures.
        let quantized_upper_bound = MAX_RECORDED_MICROS + MAX_RECORDED_MICROS / 100;
        for observed_max in [
            metrics.dispatch_wait.max(),
            metrics.scheduled_to_completion.max(),
        ] {
            assert!(observed_max >= MAX_RECORDED_MICROS);
            assert!(observed_max <= quantized_upper_bound);
        }
    }
}
