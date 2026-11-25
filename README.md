# Invite Monitor

A Windows-friendly Rust monitor for observing public Discord invite pages. It is a monitoring and benchmarking tool only: it does not authenticate to Discord accounts, claim vanity URLs, or automate account actions.

## Features

- One Tokio runtime for the lifetime of the process.
- One shared Reqwest client with connection pooling.
- Bounded worker concurrency.
- Global request pacing and per-target polling intervals.
- Retry-After handling for HTTP 429 responses.
- Bounded response-body reads.
- HTML metadata parsing instead of brittle exact-string matching.
- Structured logs and P50/P95/P99 HTTP observation latency.
- Graceful Ctrl+C shutdown.

## Build on Windows

Install the stable Rust toolchain, then:

```powershell
cargo build --release
```

Copy the example files:

```powershell
Copy-Item config.example.toml config.toml
Copy-Item urls.example.txt urls.txt
```

Edit urls.txt and add one public invite code per line. Run:

```powershell
$env:RUST_LOG = "info"
cargo run --release
```

## Configuration

- interval_ms: per-target polling interval. The program enforces at least 5,000 ms.
- global_min_spacing_ms: minimum time between requests to the service. The program enforces at least 1,000 ms.
- concurrency: maximum number of in-flight observations, clamped to 1–4.
- timeout_secs: request timeout, clamped to 2–30 seconds.
- max_body_bytes: maximum response body size, clamped to 32 KiB–2 MiB.

These conservative limits are intentional. Do not increase request frequency to evade service rate limits. The monitor honors Retry-After and applies a shared cooldown.

## Interpreting results

The old implementation searched for a Discord metadata marker. This version parses the marker as HTML metadata. Finding it is reported only as an **invalid-invite candidate**. Not finding it is inconclusive. Neither result is an authoritative determination that a guild vanity URL is claimable.

HTTP latency is measured from request start until the response body has been read and classified. It is not the same as change-to-detection latency, and it does not include any claim operation.

## Benchmarking

Use a controlled local HTTP fixture or an endpoint you own to benchmark request, body-read, parsing, and event-processing costs. Do not use high-frequency polling against Discord as a load test.
