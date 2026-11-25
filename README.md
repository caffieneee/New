# Invite Monitor

A Windows-friendly Rust monitor for observing public Discord invite pages. It is a monitoring and benchmarking tool only: it does not authenticate to Discord accounts, claim vanity URLs, or automate account actions.

## Architecture

- config.rs: configuration parsing and conservative bounds.
- targets.rs: target validation, deduplication, and URL construction.
- observer.rs: pooled HTTP client, bounded response reads, Retry-After parsing, and HTML classification.
- metrics.rs: HDR histograms and shutdown summaries.
- main.rs: Tokio scheduling, shared request gate, bounded concurrency, cancellation, and lifecycle.

## Features

- One Tokio runtime and one shared Reqwest client.
- Bounded worker concurrency and one global request-start gate.
- Per-target polling intervals with missed-tick skipping.
- Shared cooldown on HTTP 429 responses; honors Retry-After seconds or HTTP dates.
- Bounded response-body reads and early stop after the HTML head.
- Cached HTML selector and allocation-conscious closing-head scanner.
- Separate P50/P95/P99/max metrics for semaphore wait, request-gate wait, observation duration, and scheduled-to-completion duration.
- Graceful Ctrl+C cancellation of workers and in-flight observations.

## Build on Windows

Install stable Rust, then:

```powershell
cargo build --release
Copy-Item config.example.toml config.toml
Copy-Item urls.example.txt urls.txt
$env:RUST_LOG = "info"
cargo run --release
```

## Configuration

| Setting | Default | Enforced range |
|---|---:|---:|
| interval_ms | 15000 | minimum 5000 |
| global_min_spacing_ms | 1000 | minimum 1000 |
| concurrency | 2 | 1–4 |
| timeout_secs | 8 | 2–30 |
| max_body_bytes | 262144 | 32 KiB–2 MiB |

The request floors are deliberate. Do not increase polling frequency or rotate proxies to evade service rate limits. HTTP 429 responses pause the shared request scheduler for the supplied Retry-After duration, or 30 seconds when absent.

## Interpreting results

The twitter:creator=@discord marker is only an invalid-invite candidate heuristic. Its absence is inconclusive. Neither observation authoritatively determines whether a code is available or claimable.

Observation duration includes the request, bounded response read, and HTML parsing. Scheduled-to-completion also includes time queued for a semaphore permit and the shared request gate. These measurements do not include any claim operation or represent end-to-end change-detection latency.

## Benchmarking

Run cargo bench --bench monitor_pipeline on a local machine. The loopback HTTP benchmark measures local client/server overhead only; it is not a measurement of Discord network latency. Do not use Discord as a high-frequency load-test target.
