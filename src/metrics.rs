use std::{sync::{Arc, Mutex}, time::Duration};

use hdrhistogram::Histogram;
use tracing::info;

pub struct LatencyMetrics {
    semaphore_wait: Histogram<u64>,
    request_gate_wait: Histogram<u64>,
    observation: Histogram<u64>,
    scheduled_to_completion: Histogram<u64>,
}

impl LatencyMetrics {
    pub fn new() -> Result<Self, hdrhistogram::CreationError> {
        Ok(Self {
            semaphore_wait: Histogram::new(3)?,
            request_gate_wait: Histogram::new(3)?,
            observation: Histogram::new(3)?,
            scheduled_to_completion: Histogram::new(3)?,
        })
    }

    pub fn record(&mut self, semaphore_wait: Duration, gate_wait: Duration,
                  observation: Duration, total: Duration) {
        fn micros(duration: Duration) -> u64 {
            duration.as_micros().max(1).min(u64::MAX as u128) as u64
        }
        let _ = self.semaphore_wait.record(micros(semaphore_wait));
        let _ = self.request_gate_wait.record(micros(gate_wait));
        let _ = self.observation.record(micros(observation));
        let _ = self.scheduled_to_completion.record(micros(total));
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

pub type SharedMetrics = Arc<Mutex<LatencyMetrics>>;

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
}
