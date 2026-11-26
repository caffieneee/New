# Invite Monitor

A Windows-friendly Rust monitor for observing public Discord invite pages. It is a monitoring and benchmarking tool only: it does not authenticate to Discord accounts, claim vanity URLs, or automate account actions.

## Architecture

- config.rs: configuration parsing and conservative bounds.
- targets.rs: target validation, deduplication, and URL construction.
- observer.rs: pooled HTTP client, conditional requests, bounded response reads, Retry-After parsing, and HTML classification.
- metrics.rs: HDR histograms and shutdown summaries.
- main.rs: Tokio scheduling, shared request gate, bounded concurrency, cancellation, and lifecycle.

## Features

- One Tokio runtime and one shared Reqwest client.
- Bounded worker concurrency and one global request-start gate.
- Priority watchlist: the first three entries in `urls.txt` use a 5-second timer by default; remaining entries use `interval_ms` (15 seconds by default).
- Per-target polling intervals with missed-tick skipping.
- Per-target ETag and Last-Modified validators; unchanged pages can return 304 without downloading or parsing the HTML head again.
- TCP keepalive and pooled connections to reduce avoidable reconnect overhead.
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
| interval_ms | 15000 | minimum 5000 (non-priority targets) |
| priority_targets | 3 | 1–5 |
| priority_interval_ms | 5000 | minimum 5000 |
| global_min_spacing_ms | 1000 | minimum 1000 |
| concurrency | 2 | 1–4 |
| timeout_secs | 8 | 2–30 |
| max_body_bytes | 262144 | 32 KiB–2 MiB |

The first `priority_targets` entries in `urls.txt` form the priority watchlist (default: first 3). Put the most important codes first. Priority polling uses `priority_interval_ms` (default 5 seconds); remaining targets use `interval_ms`. All targets still share the global request-start spacing and 429 cooldown, so a 5-second timer does not guarantee a completed request every 5 seconds. The enforced 1-second global spacing remains in place. Do not rotate proxies to evade service rate limits. HTTP 429 responses pause the shared request scheduler for the supplied Retry-After duration, or 30 seconds when absent.

## Interpreting results

The twitter:creator=@discord marker is only an invalid-invite candidate heuristic. Its absence is inconclusive. Neither observation authoritatively determines whether a code is available or claimable.

Observation duration includes the request, bounded response read, and HTML parsing. Scheduled-to-completion also includes time queued for a semaphore permit and the shared request gate. These measurements do not include any claim operation or represent end-to-end change-detection latency. Conditional requests can reduce transferred bytes and parsing work, but cannot remove network round-trip time.

## Benchmarking

Run cargo bench --bench monitor_pipeline on a local machine. The loopback HTTP benchmark measures local client/server overhead only; it is not a measurement of Discord network latency. Do not use Discord as a high-frequency load-test target.

## GitHub Actions runtime

The repository includes a separate rolling runtime workflow at
`.github/workflows/monitor.yml`. Start the first session manually from
**Actions → Invite Monitor Runtime → Run workflow**. The workflow builds the
release binary, runs the monitor for about 5 hours 30 minutes, queues one
successor before shutdown, and exits gracefully so the queued session can
start. The CI workflow remains independent.

Before starting it, add these repository Actions secrets under
**Settings → Secrets and variables → Actions**:

- `INVITE_CODES`: newline-separated public invite codes to observe.
- `MONITOR_CONFIG_TOML` (optional): the contents of a TOML config file.
  If omitted, the conservative built-in defaults are used.

The workflow uses the automatically provided `GITHUB_TOKEN` with
`actions: write` to dispatch its successor; no personal access token is
needed. The runtime concurrency group prevents overlapping sessions and
keeps the successor queued until the current session ends.

GitHub-hosted runners are ephemeral and successor startup is not guaranteed
to be gap-free. This is a best-effort rolling monitor, not an always-on VPS.
GitHub Actions runner placement and network latency vary between sessions.
The monitor only observes public invite pages, respects the configured
request floor and Discord `Retry-After` responses, and does not claim or
modify invite codes.
