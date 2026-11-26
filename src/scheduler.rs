use std::{error::Error, sync::Arc, time::Duration};

use tokio::{
    task::JoinSet,
    time::{self, Instant},
};
use tracing::{info, warn};

use crate::{
    config::Config,
    metrics::LatencyMetrics,
    observer::{observe, Observation, Validators},
    targets::Target,
};

const MAX_PRIORITY_BURST: usize = 5;

struct TargetState {
    target: Target,
    priority: bool,
    interval: Duration,
    next_due: Instant,
    in_flight: bool,
    validators: Validators,
    metrics: LatencyMetrics,
}

type CompletedObservation = (
    usize,
    Validators,
    Observation,
    Duration,
    Instant,
    Instant,
);

fn choose_due_index(
    states: &[TargetState],
    now: Instant,
    priority_streak: usize,
) -> Option<usize> {
    let priority = states
        .iter()
        .enumerate()
        .filter(|(_, state)| state.priority && !state.in_flight && state.next_due <= now)
        .min_by_key(|(index, state)| (state.next_due, *index))
        .map(|(index, _)| index);

    let regular = states
        .iter()
        .enumerate()
        .filter(|(_, state)| !state.priority && !state.in_flight && state.next_due <= now)
        .min_by_key(|(index, state)| (state.next_due, *index))
        .map(|(index, _)| index);

    if priority_streak >= MAX_PRIORITY_BURST {
        regular.or(priority)
    } else {
        priority.or(regular)
    }
}

fn log_observation(target: &Target, observation: &Observation, elapsed: Duration) {
    match observation {
        Observation::InvalidInviteCandidate => warn!(
            code = %target.code,
            elapsed_ms = elapsed.as_secs_f64() * 1000.0,
            "invalid-invite metadata candidate observed; not proof of availability"
        ),
        Observation::MarkerAbsent => tracing::debug!(
            code = %target.code,
            elapsed_ms = elapsed.as_secs_f64() * 1000.0,
            "metadata marker absent; result is inconclusive"
        ),
        Observation::NotModified => tracing::trace!(
            code = %target.code,
            elapsed_ms = elapsed.as_secs_f64() * 1000.0,
            "page unchanged; server returned 304 Not Modified"
        ),
        Observation::RateLimited { .. } => {}
        Observation::HttpError(status) => warn!(code = %target.code, %status, "HTTP error"),
        Observation::NetworkError(error) => warn!(code = %target.code, %error, "network error"),
        Observation::BodyTooLarge => warn!(code = %target.code, "response exceeded body cap"),
    }
}

pub async fn run(
    targets: Vec<Target>,
    client: reqwest::Client,
    config: Arc<Config>,
) -> Result<LatencyMetrics, Box<dyn Error>> {
    let now = Instant::now();
    let mut states = Vec::with_capacity(targets.len());

    for (index, target) in targets.into_iter().enumerate() {
        let priority = index < config.priority_targets;
        let interval = if priority {
            Duration::from_millis(config.priority_interval_ms)
        } else {
            config.interval()
        };
        info!(
            code = %target.code,
            priority,
            poll_interval_ms = interval.as_millis(),
            "target registered with central dispatcher"
        );
        states.push(TargetState {
            target,
            priority,
            interval,
            next_due: now,
            in_flight: false,
            validators: Validators::default(),
            metrics: LatencyMetrics::new()?,
        });
    }

    let mut tasks: JoinSet<CompletedObservation> = JoinSet::new();
    let mut in_flight = 0usize;
    let mut priority_streak = 0usize;
    let mut next_request_at = now;
    let mut cooldown_until = now;
    let mut shutdown = tokio::signal::ctrl_c();
    tokio::pin!(shutdown);

    info!(
        target_count = states.len(),
        priority_targets = config.priority_targets.min(states.len()),
        concurrency = config.concurrency,
        global_spacing_ms = config.global_min_spacing_ms,
        "central priority dispatcher started"
    );

    loop {
        // Dispatch due targets while capacity and the shared request budget permit.
        loop {
            if in_flight >= config.concurrency {
                break;
            }

            let dispatch_at = Instant::now();
            let allowed_at = if next_request_at > cooldown_until {
                next_request_at
            } else {
                cooldown_until
            };
            if dispatch_at < allowed_at {
                break;
            }

            let Some(index) = choose_due_index(&states, dispatch_at, priority_streak) else {
                break;
            };

            let state = &mut states[index];
            let scheduled_at = state.next_due;
            let was_priority = state.priority;
            let target = state.target.clone();
            let interval = state.interval;
            let mut validators = std::mem::take(&mut state.validators);
            let request_client = client.clone();
            let max_body_bytes = config.max_body_bytes;
            let request_started = Instant::now();

            // Skip missed periods instead of creating a burst of catch-up requests.
            state.next_due = request_started + interval;
            state.in_flight = true;

            tasks.spawn(async move {
                let (observation, elapsed) =
                    observe(&request_client, &target, max_body_bytes, &mut validators).await;
                (
                    index,
                    validators,
                    observation,
                    elapsed,
                    scheduled_at,
                    request_started,
                )
            });

            in_flight += 1;
            next_request_at = request_started + config.global_spacing();
            if was_priority {
                priority_streak += 1;
            } else {
                priority_streak = 0;
            }
        }

        let now = Instant::now();
        let next_due = states
            .iter()
            .filter(|state| !state.in_flight)
            .map(|state| state.next_due)
            .min();

        let wake_at = if in_flight >= config.concurrency {
            now + Duration::from_secs(86_400)
        } else if let Some(due) = next_due {
            if due <= now {
                let allowed_at = if next_request_at > cooldown_until {
                    next_request_at
                } else {
                    cooldown_until
                };
                allowed_at.max(now)
            } else {
                due
            }
        } else {
            now + Duration::from_secs(86_400)
        };

        tokio::select! {
            _ = &mut shutdown => {
                info!("Ctrl+C received; cancelling in-flight observations");
                tasks.abort_all();
                while tasks.join_next().await.is_some() {}
                break;
            }
            completed = tasks.join_next(), if !tasks.is_empty() => {
                if let Some(result) = completed {
                    match result {
                        Ok((index, validators, observation, elapsed, scheduled_at, request_started)) => {
                            in_flight = in_flight.saturating_sub(1);
                            let state = &mut states[index];
                            state.in_flight = false;
                            state.validators = validators;

                            let dispatch_wait = request_started.saturating_duration_since(scheduled_at);
                            let scheduled_to_completion = scheduled_at.elapsed();
                            state.metrics.record(dispatch_wait, elapsed, scheduled_to_completion);

                            if let Observation::RateLimited { retry_after } = &observation {
                                let cooldown = retry_after
                                    .unwrap_or(Duration::from_secs(30))
                                    .max(Duration::from_secs(1));
                                let requested = Instant::now() + cooldown;
                                if requested > cooldown_until {
                                    cooldown_until = requested;
                                }
                                warn!(
                                    code = %state.target.code,
                                    cooldown_ms = cooldown.as_millis(),
                                    "rate limited; central dispatcher paused until Retry-After"
                                );
                            }

                            log_observation(&state.target, &observation, elapsed);
                        }
                        Err(error) => {
                            in_flight = in_flight.saturating_sub(1);
                            warn!(%error, "observation task failed");
                        }
                    }
                }
            }
            _ = time::sleep_until(wake_at) => {}
        }
    }

    let mut aggregate = LatencyMetrics::new()?;
    for state in states {
        info!(code = %state.target.code, "monitor stopped");
        aggregate.merge(state.metrics);
    }
    aggregate.log_summary();
    info!("central priority dispatcher stopped cleanly");
    Ok(aggregate)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(code: &str, priority: bool, next_due: Instant) -> TargetState {
        TargetState {
            target: Target {
                code: code.to_owned(),
                url: url::Url::parse(&format!("https://discord.com/invite/{code}")).unwrap(),
            },
            priority,
            interval: Duration::from_secs(5),
            next_due,
            in_flight: false,
            validators: Validators::default(),
            metrics: LatencyMetrics::new().unwrap(),
        }
    }

    #[test]
    fn due_priority_targets_are_selected_first() {
        let now = Instant::now();
        let states = vec![
            state("regular", false, now),
            state("priority", true, now),
        ];
        assert_eq!(choose_due_index(&states, now, 0), Some(1));
    }

    #[test]
    fn regular_targets_get_a_turn_after_five_priority_dispatches() {
        let now = Instant::now();
        let states = vec![
            state("priority-a", true, now),
            state("regular", false, now),
            state("priority-b", true, now),
        ];
        assert_eq!(choose_due_index(&states, now, MAX_PRIORITY_BURST), Some(1));
    }

    #[test]
    fn in_flight_and_not_yet_due_targets_are_skipped() {
        let now = Instant::now();
        let mut busy = state("busy", true, now);
        busy.in_flight = true;
        let future = state("future", true, now + Duration::from_secs(1));
        let ready = state("ready", false, now);
        let states = vec![busy, future, ready];
        assert_eq!(choose_due_index(&states, now, 0), Some(2));
    }
}
