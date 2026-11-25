# Performance and measurement guide

## What the latency metrics mean

The process reports four HDR histogram summaries on shutdown:

- **Semaphore wait**: time from a target's scheduled tick until it obtains a worker permit.
- **Request-gate wait**: time spent waiting for the shared minimum request-start spacing.
- **Observation**: request send, response receive, bounded body read, and HTML classification.
- **Scheduled-to-completion**: total elapsed time from Tokio's scheduled tick instant through observation completion.

The four values are not additive in every scenario because scheduling and asynchronous wake-up overhead overlap. Compare distributions across repeated runs rather than subtracting individual quantiles.

## Interpreting a 20 ms target

A sub-20 ms local parsing or scheduler operation can be measurable on a suitable machine. A sub-20 ms end-to-end public-page detection guarantee is not realistic: remote network round-trip time, server response time, DNS/TLS setup, polling cadence, and scheduling all contribute. The configured polling interval is intentionally no lower than 5 seconds and the shared request-start spacing no lower than 1 second.

Do not use proxy rotation, parallel account sessions, or faster polling to evade service limits. Respect HTTP 429 and Retry-After.

## Benchmark sequence

1. Run the Criterion suite on the same Windows machine and power profile used for development.
2. Record compiler version, CPU, OS, and whether the build is release mode.
3. Compare the scanner and parser benchmarks independently from the loopback HTTP benchmark.
4. Run the monitor against a controlled endpoint you own when measuring network behavior.
5. Repeat runs and compare P50/P95/P99; avoid conclusions from one sample.

The loopback HTTP benchmark measures local client/server overhead only. It is not a measurement of Discord's network latency.

## Practical optimization order

1. Confirm whether delay is local queueing, request pacing, network, or HTML processing using the four metrics.
2. Keep a shared HTTP client and connection pool; avoid rebuilding clients per observation.
3. Keep response reads bounded and stop after the document head when the marker is present there.
4. Avoid expensive per-request logging in the hot path.
5. Change concurrency or memory behavior only after measurements show a bottleneck, while preserving rate-limit handling.
