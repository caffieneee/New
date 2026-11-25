use std::{
    collections::HashSet,
    error::Error,
    fs,
    path::Path,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime},
};

use hdrhistogram::Histogram;
use reqwest::{header::RETRY_AFTER, Client, StatusCode};
use scraper::{Html, Selector};
use serde::Deserialize;
use tokio::{
    sync::{watch, Mutex as AsyncMutex, Semaphore},
    task::JoinSet,
    time::{self, Instant, MissedTickBehavior},
};
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;
use url::Url;

const MIN_TARGET_INTERVAL_MS: u64 = 5_000;
const MIN_GLOBAL_SPACING_MS: u64 = 1_000;
const MAX_TARGETS: usize = 1_000;
const MARKER_SELECTOR: &str = r#"meta[name="twitter:creator"][content="@discord"]"#;

#[derive(Debug, Deserialize)]
#[serde(default)]
struct Config {
    interval_ms: u64,
    global_min_spacing_ms: u64,
    concurrency: usize,
    timeout_secs: u64,
    max_body_bytes: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            interval_ms: 15_000,
            global_min_spacing_ms: MIN_GLOBAL_SPACING_MS,
            concurrency: 2,
            timeout_secs: 8,
            max_body_bytes: 256 * 1024,
        }
    }
}

#[derive(Debug, Clone)]
struct Target {
    code: String,
    url: Url,
}

#[derive(Debug)]
enum Observation {
    InvalidInviteCandidate,
    MarkerAbsent,
    RateLimited { retry_after: Option<Duration> },
    HttpError(StatusCode),
    NetworkError(String),
    BodyTooLarge,
    InvalidHtml,
}

struct RequestGate {
    next_allowed: AsyncMutex<Instant>,
    spacing: Duration,
}

impl RequestGate {
    fn new(spacing: Duration) -> Self {
        Self {
            next_allowed: AsyncMutex::new(Instant::now()),
            spacing,
        }
    }

    async fn wait_turn(&self) {
        // Holding this small async mutex while waiting serializes request starts
        // and enforces one shared per-origin pacing limit across all targets.
        let mut next = self.next_allowed.lock().await;
        let now = Instant::now();
        if *next > now {
            time::sleep_until(*next).await;
        }
        *next = Instant::now() + self.spacing;
    }

    async fn extend_cooldown(&self, delay: Duration) {
        let mut next = self.next_allowed.lock().await;
        let requested = Instant::now() + delay;
        if requested > *next {
            *next = requested;
        }
    }
}

fn load_config() -> Result<Config, Box<dyn Error>> {
    if !Path::new("config.toml").exists() {
        warn!("config.toml not found; using conservative built-in defaults");
        return Ok(Config::default());
    }

    let raw = fs::read_to_string("config.toml")?;
    let mut config: Config = toml::from_str(&raw)?;

    config.interval_ms = config.interval_ms.max(MIN_TARGET_INTERVAL_MS);
    config.global_min_spacing_ms = config
        .global_min_spacing_ms
        .max(MIN_GLOBAL_SPACING_MS);
    config.concurrency = config.concurrency.clamp(1, 4);
    config.timeout_secs = config.timeout_secs.clamp(2, 30);
    config.max_body_bytes = config.max_body_bytes.clamp(32 * 1024, 2 * 1024 * 1024);

    Ok(config)
}

fn load_targets() -> Result<Vec<Target>, Box<dyn Error>> {
    let raw = fs::read_to_string("urls.txt")
        .map_err(|_| "urls.txt not found; copy urls.example.txt to urls.txt")?;

    let mut seen = HashSet::new();
    let mut targets = Vec::new();

    for (line_index, line) in raw.lines().enumerate() {
        let code = line.trim();
        if code.is_empty() || code.starts_with('#') {
            continue;
        }

        if !(2..=64).contains(&code.len())
            || !code
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return Err(format!("invalid invite code on urls.txt line {}", line_index + 1).into());
        }

        if !seen.insert(code.to_owned()) {
            continue;
        }

        let url = Url::parse(&format!("https://discord.com/invite/{code}"))?;
        targets.push(Target {
            code: code.to_owned(),
            url,
        });

        if targets.len() > MAX_TARGETS {
            return Err(format!("more than {MAX_TARGETS} targets; reduce urls.txt").into());
        }
    }

    if targets.is_empty() {
        return Err("urls.txt contains no valid targets".into());
    }

    Ok(targets)
}

fn build_client(timeout_secs: u64) -> Result<Client, reqwest::Error> {
    Client::builder()
        .user_agent("public-invite-monitor/0.1")
        .connect_timeout(Duration::from_secs(3))
        .timeout(Duration::from_secs(timeout_secs))
        .pool_idle_timeout(Duration::from_secs(90))
        .pool_max_idle_per_host(4)
        .tcp_nodelay(true)
        .redirect(reqwest::redirect::Policy::limited(5))
        .build()
}

fn parse_retry_after(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    let value = headers.get(RETRY_AFTER)?.to_str().ok()?;

    if let Ok(seconds) = value.parse::<u64>() {
        return Some(Duration::from_secs(seconds));
    }

    let date = httpdate::parse_http_date(value).ok()?;
    Some(date.duration_since(SystemTime::now()).unwrap_or_default())
}

async fn observe(
    client: &Client,
    target: &Target,
    max_body_bytes: usize,
) -> (Observation, Duration) {
    let started = Instant::now();

    let response = match client.get(target.url.clone()).send().await {
        Ok(response) => response,
        Err(error) => {
            return (
                Observation::NetworkError(error.to_string()),
                started.elapsed(),
            );
        }
    };

    let status = response.status();

    if status == StatusCode::TOO_MANY_REQUESTS {
        let retry_after = parse_retry_after(response.headers());
        return (
            Observation::RateLimited { retry_after },
            started.elapsed(),
        );
    }

    if !status.is_success() {
        return (Observation::HttpError(status), started.elapsed());
    }

    if response
        .content_length()
        .is_some_and(|length| length > max_body_bytes as u64)
    {
        return (Observation::BodyTooLarge, started.elapsed());
    }

    let mut response = response;
    let mut body = Vec::with_capacity(
        response
            .content_length()
            .unwrap_or(0)
            .min(max_body_bytes as u64) as usize,
    );

    loop {
        match response.chunk().await {
            Ok(Some(chunk)) => {
                if body.len().saturating_add(chunk.len()) > max_body_bytes {
                    return (Observation::BodyTooLarge, started.elapsed());
                }
                body.extend_from_slice(&chunk);
            }
            Ok(None) => break,
            Err(error) => {
                return (
                    Observation::NetworkError(error.to_string()),
                    started.elapsed(),
                );
            }
        }
    }

    let html = String::from_utf8_lossy(&body);
    let document = Html::parse_document(&html);
    let selector = match Selector::parse(MARKER_SELECTOR) {
        Ok(selector) => selector,
        Err(_) => return (Observation::InvalidHtml, started.elapsed()),
    };

    let observation = if document.select(&selector).next().is_some() {
        Observation::InvalidInviteCandidate
    } else {
        // Absence of the marker is not proof that a code is taken or available.
        Observation::MarkerAbsent
    };

    (observation, started.elapsed())
}

async fn monitor_target(
    target: Target,
    client: Client,
    config: Arc<Config>,
    gate: Arc<RequestGate>,
    semaphore: Arc<Semaphore>,
    histogram: Arc<Mutex<Histogram<u64>>>,
    mut shutdown: watch::Receiver<bool>,
) {
    let interval = Duration::from_millis(config.interval_ms);
    let mut ticker = time::interval(interval);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    break;
                }
            }
            _ = ticker.tick() => {}
        }

        let permit = tokio::select! {
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    break;
                }
                continue;
            }
            permit = semaphore.acquire() => match permit {
                Ok(permit) => permit,
                Err(_) => break,
            }
        };

        gate.wait_turn().await;

        let (observation, elapsed) =
            observe(&client, &target, config.max_body_bytes).await;

        if let Observation::RateLimited { retry_after } = &observation {
            let cooldown = retry_after.unwrap_or(Duration::from_secs(30));
            gate.extend_cooldown(cooldown.max(Duration::from_secs(1))).await;
            warn!(
                code = %target.code,
                cooldown_ms = cooldown.as_millis(),
                "rate limited; pausing shared request scheduler"
            );
        }

        if let Ok(mut histogram) = histogram.lock() {
            let micros = elapsed.as_micros().max(1).min(u64::MAX as u128) as u64;
            let _ = histogram.record(micros);
        }

        match &observation {
            Observation::InvalidInviteCandidate => {
                warn!(
                    code = %target.code,
                    elapsed_ms = elapsed.as_secs_f64() * 1000.0,
                    "public-page invalid-invite marker observed; candidate only"
                );
            }
            Observation::MarkerAbsent => {
                info!(
                    code = %target.code,
                    elapsed_ms = elapsed.as_secs_f64() * 1000.0,
                    "marker absent; result is inconclusive"
                );
            }
            Observation::RateLimited { .. } => {}
            Observation::HttpError(status) => {
                warn!(code = %target.code, %status, "HTTP response was not successful");
            }
            Observation::NetworkError(error) => {
                warn!(code = %target.code, %error, "network observation failed");
            }
            Observation::BodyTooLarge => {
                warn!(code = %target.code, "response exceeded configured body limit");
            }
            Observation::InvalidHtml => {
                warn!(code = %target.code, "HTML selector could not be initialized");
            }
        }

        drop(permit);
    }

    info!(code = %target.code, "monitor stopped");
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_target(false)
        .init();

    let config = Arc::new(load_config()?);
    let targets = load_targets()?;
    let client = build_client(config.timeout_secs)?;

    info!(
        target_count = targets.len(),
        interval_ms = config.interval_ms,
        global_spacing_ms = config.global_min_spacing_ms,
        concurrency = config.concurrency,
        "starting public invite-page monitor"
    );

    let gate = Arc::new(RequestGate::new(Duration::from_millis(
        config.global_min_spacing_ms,
    )));
    let semaphore = Arc::new(Semaphore::new(config.concurrency));
    let histogram = Arc::new(Mutex::new(Histogram::<u64>::new(3)?));
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let mut tasks = JoinSet::new();

    for target in targets {
        tasks.spawn(monitor_target(
            target,
            client.clone(),
            Arc::clone(&config),
            Arc::clone(&gate),
            Arc::clone(&semaphore),
            Arc::clone(&histogram),
            shutdown_rx.clone(),
        ));
    }

    tokio::signal::ctrl_c().await?;
    info!("Ctrl+C received; stopping workers");
    let _ = shutdown_tx.send(true);

    while let Some(result) = tasks.join_next().await {
        if let Err(error) = result {
            warn!(%error, "monitor worker exited unexpectedly");
        }
    }

    if let Ok(histogram) = histogram.lock() {
        if histogram.len() > 0 {
            info!(
                samples = histogram.len(),
                p50_us = histogram.value_at_quantile(0.50),
                p95_us = histogram.value_at_quantile(0.95),
                p99_us = histogram.value_at_quantile(0.99),
                max_us = histogram.max(),
                "HTTP observation latency summary"
            );
        }
    }

    info!("monitor stopped cleanly");
    Ok(())
}
