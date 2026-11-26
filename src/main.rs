mod config;
mod metrics;
mod observer;
mod scheduler;
mod targets;

use std::{error::Error, sync::Arc};

use tracing::warn;
use tracing_subscriber::EnvFilter;

use config::Config;
use scheduler::run as run_scheduler;
use targets::load_targets;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .with_target(false)
        .init();

    let config = Arc::new(Config::load()?);
    let targets = load_targets()?;
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

    let client = observer::build_client(config.timeout_secs)?;
    let _metrics = run_scheduler(targets, client, config).await?;
    Ok(())
}
