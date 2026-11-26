mod config;
mod metrics;
mod observer;
mod targets;

use std::{
    error::Error,
    sync::Arc,
    time::Duration,
};

use metrics::LatencyMetrics;
use observer::{build_client, observe, Observation, Validators};
use tokio::{
    sync::{watch, Mutex as AsyncMutex, Semaphore},
    task::JoinSet,
    time::{self, Instant, MissedTickBehavior},
};
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

use config::Config;
use targets::{load_targets, Target};

struct RequestGate {
    next_allowed: AsyncMutex<Instant>,
    spacing: Duration,
}

impl RequestGate {
    fn new(spacing: Duration) -> Self {
        Self { next_allowed: AsyncMutex::new(Instant::now()), spacing }
    }

    async fn wait_turn(&self) -> Duration {
        let started = Instant::now();
        loop {
            let wait_until = {
                let mut next = self.next_allowed.lock().await;
                let now = Instant::now();
                if *next <= now {
                    *next = now + self.spacing;
                    return started.elapsed();
                }
                *next
            };
            time::sleep_until(wait_until).await;
        }
    }

    async fn extend_cooldown(&self, delay: Duration) {
        let mut next = self.next_allowed.lock().await;
        let requested = Instant::now() + delay;
        if requested > *next {
            *next = requested;
        }
    }
}

async fn monitor_target(
    target: Target,
    client: reqwest::Client,
    config: Arc<Config>,
    gate: Arc<RequestGate>,
    semaphore: Arc<Semaphore>,
    poll_interval: Duration,
    mut shutdown: watch::Receiver<bool>,
) -> LatencyMetrics {
    // Each worker owns its histograms. This removes a shared std::sync::Mutex
    // acquisition from every observation and keeps async workers non-blocking.
    let mut metrics = LatencyMetrics::new().expect("valid HDR histogram configuration");
    let mut validators = Validators::default();
    let mut ticker = time::interval(poll_interval);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);

    loop {
        let scheduled_at = tokio::select! {
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() { break; }
                continue;
            }
            tick = ticker.tick() => tick,
        };

        let semaphore_started = Instant::now();
        let permit = tokio::select! {
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() { break; }
                continue;
            }
            permit = semaphore.acquire() => match permit {
                Ok(permit) => permit,
                Err(_) => break,
            }
        };
        let semaphore_wait = semaphore_started.elapsed();

        let gate_wait = tokio::select! {
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() { break; }
                continue;
            }
            wait = gate.wait_turn() => wait,
        };

        let (observation, observation_elapsed) = tokio::select! {
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() { break; }
                continue;
            }
            result = observe(&client, &target, config.max_body_bytes, &mut validators) => result,
        };
        let total_elapsed = scheduled_at.elapsed();

        if let Observation::RateLimited { retry_after } = &observation {
            let cooldown = retry_after.unwrap_or(Duration::from_secs(30))
                .max(Duration::from_secs(1));
            gate.extend_cooldown(cooldown).await;
            warn!(
                code = %target.code,
                cooldown_ms = cooldown.as_millis(),
                "rate limited; shared request scheduler paused"
            );
        }

        metrics.record(semaphore_wait, gate_wait, observation_elapsed, total_elapsed);

        match &observation {
            Observation::InvalidInviteCandidate => warn!(
                code = %target.code,
                elapsed_ms = observation_elapsed.as_secs_f64() * 1000.0,
                "invalid-invite metadata candidate observed; not proof of availability"
            ),
            Observation::MarkerAbsent => tracing::debug!(
                code = %target.code,
                elapsed_ms = observation_elapsed.as_secs_f64() * 1000.0,
                "metadata marker absent; result is inconclusive"
            ),
            Observation::NotModified => tracing::trace!(
                code = %target.code,
                elapsed_ms = observation_elapsed.as_secs_f64() * 1000.0,
                "page unchanged; server returned 304 Not Modified"
            ),
            Observation::RateLimited { .. } => {}
            Observation::HttpError(status) => warn!(code = %target.code, %status, "HTTP error"),
            Observation::NetworkError(error) => warn!(code = %target.code, %error, "network error"),
            Observation::BodyTooLarge => warn!(code = %target.code, "response exceeded body cap"),
        }

        drop(permit);
    }

    info!(code = %target.code, "monitor stopped");
    metrics
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .with_target(false)
        .init();

    let config = Arc::new(Config::load()?);
    let targets = load_targets()?;
    let client = build_client(config.timeout_secs)?;

    let estimated_sweep = config.global_spacing().saturating_mul(targets.len() as u32);
    if estimated_sweep > config.interval() {
        warn!(
            estimated_sweep_secs = estimated_sweep.as_secs_f64(),
            configured_interval_secs = config.interval().as_secs_f64(),
            target_count = targets.len(),
        priority_targets = config.priority_targets.min(targets.len()),
        priority_interval_ms = config.priority_interval_ms,
            "configured request budget cannot complete a full target sweep within one interval"
        );
    }

    info!(
        target_count = targets.len(),
        interval_ms = config.interval_ms,
        global_spacing_ms = config.global_min_spacing_ms,
        concurrency = config.concurrency,
        timeout_secs = config.timeout_secs,
        max_body_bytes = config.max_body_bytes,
        "starting public invite-page monitor"
    );

    let gate = Arc::new(RequestGate::new(config.global_spacing()));
    let semaphore = Arc::new(Semaphore::new(config.concurrency));
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let mut tasks = JoinSet::new();

    for (index, target) in targets.into_iter().enumerate() {
        let is_priority = index < config.priority_targets;
        let poll_interval = if is_priority {
            Duration::from_millis(config.priority_interval_ms)
        } else {
            config.interval()
        };
        info!(code = %target.code, priority = is_priority, poll_interval_ms = poll_interval.as_millis(), "target scheduled");
        tasks.spawn(monitor_target(
            target,
            client.clone(),
            Arc::clone(&config),
            Arc::clone(&gate),
            Arc::clone(&semaphore),
            poll_interval,
            shutdown_rx.clone(),
        ));
    }

    tokio::signal::ctrl_c().await?;
    info!("Ctrl+C received; cancelling in-flight observations");
    let _ = shutdown_tx.send(true);

    let mut aggregate_metrics = LatencyMetrics::new()?;
    while let Some(result) = tasks.join_next().await {
        match result {
            Ok(worker_metrics) => aggregate_metrics.merge(worker_metrics),
            Err(error) => warn!(%error, "monitor worker exited unexpectedly"),
        }
    }

    aggregate_metrics.log_summary();
    info!("monitor stopped cleanly");
    Ok(())
}

#[cfg(test)]
mod scheduler_tests {
    use super::*;

    #[tokio::test]
    async fn cooldown_extends_a_waiting_request_deadline() {
        let gate = Arc::new(RequestGate::new(Duration::from_millis(10)));
        let _ = gate.wait_turn().await;

        let waiting_gate = Arc::clone(&gate);
        let waiter = tokio::spawn(async move {
            let started = Instant::now();
            let _ = waiting_gate.wait_turn().await;
            started.elapsed()
        });

        time::sleep(Duration::from_millis(2)).await;
        gate.extend_cooldown(Duration::from_millis(25)).await;
        let waited = waiter.await.unwrap();
        assert!(waited >= Duration::from_millis(20));
    }
}
