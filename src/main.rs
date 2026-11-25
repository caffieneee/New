mod config;
mod metrics;
mod observer;
mod targets;

use std::{
    error::Error,
    sync::{Arc, Mutex},
    time::Duration,
};

use metrics::{LatencyMetrics, SharedMetrics};
use observer::{build_client, observe, Observation};
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
        let mut next = self.next_allowed.lock().await;
        let now = Instant::now();
        if *next > now {
            time::sleep_until(*next).await;
        }
        *next = Instant::now() + self.spacing;
        started.elapsed()
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
    metrics: SharedMetrics,
    mut shutdown: watch::Receiver<bool>,
) {
    let mut ticker = time::interval(config.interval());
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
            result = observe(&client, &target, config.max_body_bytes) => result,
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

        if let Ok(mut metrics) = metrics.lock() {
            metrics.record(semaphore_wait, gate_wait, observation_elapsed, total_elapsed);
        }

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
            Observation::RateLimited { .. } => {}
            Observation::HttpError(status) => warn!(code = %target.code, %status, "HTTP error"),
            Observation::NetworkError(error) => warn!(code = %target.code, %error, "network error"),
            Observation::BodyTooLarge => warn!(code = %target.code, "response exceeded body cap"),
        }

        drop(permit);
    }

    info!(code = %target.code, "monitor stopped");
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
    let metrics: SharedMetrics = Arc::new(Mutex::new(LatencyMetrics::new()?));
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let mut tasks = JoinSet::new();

    for target in targets {
        tasks.spawn(monitor_target(
            target,
            client.clone(),
            Arc::clone(&config),
            Arc::clone(&gate),
            Arc::clone(&semaphore),
            Arc::clone(&metrics),
            shutdown_rx.clone(),
        ));
    }

    tokio::signal::ctrl_c().await?;
    info!("Ctrl+C received; cancelling in-flight observations");
    let _ = shutdown_tx.send(true);

    while let Some(result) = tasks.join_next().await {
        if let Err(error) = result {
            warn!(%error, "monitor worker exited unexpectedly");
        }
    }

    if let Ok(metrics) = metrics.lock() {
        metrics.log_summary();
    }
    info!("monitor stopped cleanly");
    Ok(())
}
