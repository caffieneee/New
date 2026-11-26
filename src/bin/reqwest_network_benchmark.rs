use std::{env, fs::File, io::{BufWriter, Write}, time::Duration};

use tokio::time::{sleep, Instant};
use tracing::{info, warn};

const MIN_REQUEST_SPACING: Duration = Duration::from_secs(5);

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let codes = env::var("INVITE_CODES").unwrap_or_default();
    let mut targets: Vec<String> = codes
        .lines()
        .map(str::trim)
        .filter(|code| !code.is_empty() && !code.contains('/') && !code.chars().any(char::is_whitespace))
        .map(str::to_owned)
        .collect();
    targets.sort();
    targets.dedup();

    if targets.is_empty() {
        return Err("INVITE_CODES is missing or contains no valid public invite codes".into());
    }

    let samples: usize = env::var("SAMPLES").unwrap_or_else(|_| "3".into()).parse()?;
    if !(1..=5).contains(&samples) {
        return Err("SAMPLES must be between 1 and 5".into());
    }

    // This is the same pooled Reqwest stack used by the monitor, not a new
    // curl process for every sample. Bodies are consumed so connections can
    // return to the pool for later requests.
    let client = reqwest::Client::builder()
        .user_agent("public-invite-monitor-benchmark/0.1")
        .connect_timeout(Duration::from_secs(3))
        .timeout(Duration::from_secs(15))
        .pool_idle_timeout(Duration::from_secs(90))
        .pool_max_idle_per_host(4)
        .tcp_nodelay(true)
        .tcp_keepalive(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::limited(5))
        .build()?;

    let file = File::create("network-latency.csv")?;
    let mut output = BufWriter::new(file);
    writeln!(output, "target,sample,connection_phase,http_code,headers_ms,body_ms,total_ms,download_bytes")?;

    let mut previous_start: Option<Instant> = None;
    let mut count = 0usize;
    let mut rate_limited = false;

    'targets: for (target_index, code) in targets.iter().enumerate() {
        let url = format!("https://discord.gg/{code}");

        for sample in 1..=samples {
            if let Some(previous) = previous_start {
                let elapsed = previous.elapsed();
                if elapsed < MIN_REQUEST_SPACING {
                    sleep(MIN_REQUEST_SPACING - elapsed).await;
                }
            }

            let started = Instant::now();
            previous_start = Some(started);
            let response = match client.get(&url).send().await {
                Ok(response) => response,
                Err(error) => {
                    warn!(
                        target = target_index + 1,
                        sample,
                        error = %error,
                        "request failed; target value suppressed"
                    );
                    continue;
                }
            };

            let headers_elapsed = started.elapsed();
            let status = response.status();
            let retry_after = response.headers().get(reqwest::header::RETRY_AFTER)
                .and_then(|value| value.to_str().ok())
                .unwrap_or("")
                .to_owned();

            let body_started = Instant::now();
            let body = match response.bytes().await {
                Ok(body) => body,
                Err(error) => {
                    warn!(
                        target = target_index + 1,
                        sample,
                        error = %error,
                        "response body read failed"
                    );
                    continue;
                }
            };
            let body_elapsed = body_started.elapsed();
            let total_elapsed = started.elapsed();

            // The label is deliberately conservative: Reqwest does not expose
            // a public per-request "socket reused" flag.
            let phase = if sample == 1 { "initial_for_target" } else { "subsequent_candidate" };
            writeln!(
                output,
                "{},{},{},{},{:.3},{:.3},{:.3},{}",
                target_index + 1,
                sample,
                phase,
                status.as_u16(),
                headers_elapsed.as_secs_f64() * 1000.0,
                body_elapsed.as_secs_f64() * 1000.0,
                total_elapsed.as_secs_f64() * 1000.0,
                body.len()
            )?;
            count += 1;

            info!(
                target = target_index + 1,
                total_targets = targets.len(),
                sample,
                samples,
                http_code = status.as_u16(),
                headers_ms = headers_elapsed.as_secs_f64() * 1000.0,
                body_ms = body_elapsed.as_secs_f64() * 1000.0,
                total_ms = total_elapsed.as_secs_f64() * 1000.0,
                download_bytes = body.len(),
                "pooled Reqwest network sample"
            );

            if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                warn!(
                    retry_after = %if retry_after.is_empty() { "unspecified" } else { &retry_after },
                    "HTTP 429 received; stopping benchmark without retry"
                );
                rate_limited = true;
                break 'targets;
            }
        }
    }

    output.flush()?;
    info!(samples_recorded = count, "network benchmark complete");
    if rate_limited {
        return Err("benchmark stopped after HTTP 429; respect Retry-After before another run".into());
    }
    Ok(())
}
