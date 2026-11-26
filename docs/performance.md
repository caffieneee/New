# Performance and measurement guide

## What the latency metrics mean

The process reports four HDR histogram summaries on shutdown:

- **Semaphore wait**: time from a target's scheduled tick until it obtains a worker permit.
- **Request-gate wait**: time spent waiting for the shared minimum request-start spacing.
- **Observation**: request send, response receive, bounded body read, and HTML classification.
- **Scheduled-to-completion**: total elapsed time from Tokio's scheduled tick instant through observation completion.

The four values are not additive in every scenario because scheduling and asynchronous wake-up overhead overlap. Compare distributions across repeated runs rather than subtracting individual quantiles.

Worker-local histograms use bounded ranges (1 microsecond through 24 hours) and two significant figures. This keeps per-target metric memory bounded; values beyond 24 hours are clamped to the upper bound.

## Interpreting a 20 ms target

A sub-20 ms local parsing or scheduler operation can be measurable on a suitable machine. A sub-20 ms end-to-end public-page detection guarantee is not realistic: remote network round-trip time, server response time, DNS/TLS setup, polling cadence, and scheduling all contribute. The configured polling interval is intentionally no lower than 5 seconds and the shared request-start spacing no lower than 1 second.

Do not use proxy rotation, parallel account sessions, or faster polling to evade service limits. Respect HTTP 429 and Retry-After.

## Network benchmark methodology

The manual Network Latency Benchmark workflow uses the same Reqwest client stack as the Rust monitor. It keeps one client alive across samples, consumes each response body so sockets can return to the connection pool, and spaces request starts by at least five seconds. This is a closer representation of production connection pooling than launching a separate curl process for every sample.

The CSV records:

- `headers_ms`: request start until response headers arrive (includes DNS, connection setup when needed, network transit, and server response time).
- `body_ms`: time to consume the response body after headers arrive.
- `total_ms`: request start through complete body consumption.
- `connection_phase`: `first_request` or `pooled_reuse_candidate`. The latter means a prior request used the shared host pool; it does not prove socket reuse because Reqwest does not expose a public per-request socket-reuse flag.

The benchmark stops on HTTP 429 and never retries. It does not print invite codes. A successful response does not prove that an invite is claimable or available.

The older `scripts/network_benchmark.py` script remains available for cold-process curl comparisons. Its separate curl processes do not share a connection pool, so do not compare its timings directly with the pooled Reqwest benchmark.

## Benchmark sequence

1. Run the Criterion suite on the same Windows machine and power profile used for development.
2. Record compiler version, CPU, OS, and whether the build is release mode.
3. Compare the scanner and parser benchmarks independently from the loopback HTTP benchmark.
4. Run the pooled Reqwest workflow against configured public pages, observing the five-second request-start floor.
5. Repeat runs and compare P50/P95/P99; avoid conclusions from one small sample.

The loopback HTTP benchmark measures local client/server overhead only. It is not a measurement of Discord's network latency.

## Practical optimization order

1. Confirm whether delay is local queueing, request pacing, network, or HTML processing using the four runtime metrics.
2. Keep a shared HTTP client and connection pool; avoid rebuilding clients per observation.
3. Keep response reads bounded and stop after the document head when the marker is present there.
4. Avoid expensive per-request logging in the hot path.
5. Change concurrency or memory behavior only after measurements show a bottleneck, while preserving request pacing and rate-limit handling.
