#!/usr/bin/env python3
"""Measure public invite-page HTTP timings without printing invite codes.

Requests are sequential and separated by at least five seconds. A 429 stops
the run immediately; this benchmark never retries or bypasses rate limits.
"""
from __future__ import annotations

import csv
import os
import subprocess
import sys
import time
from collections import defaultdict
from pathlib import Path
from statistics import median

FIELDS = (
    "http_code",
    "dns_s",
    "connect_s",
    "tls_s",
    "ttfb_s",
    "total_s",
    "download_bytes",
)
WRITE_OUT = (
    "%{http_code}\t%{time_namelookup}\t%{time_connect}\t"
    "%{time_appconnect}\t%{time_starttransfer}\t%{time_total}\t"
    "%{size_download}"
)
OUTPUT = Path("network-latency.csv")


def percentile(values: list[float], p: float) -> float:
    ordered = sorted(values)
    if not ordered:
        return 0.0
    index = max(0, min(len(ordered) - 1, int((len(ordered) - 1) * p + 0.999999)))
    return ordered[index]


def main() -> int:
    codes = [line.strip() for line in os.environ.get("INVITE_CODES", "").splitlines()]
    codes = list(dict.fromkeys(code for code in codes if code and "/" not in code))
    if not codes:
        print("Missing INVITE_CODES secret (newline-separated public invite codes).", file=sys.stderr)
        return 2

    samples = int(os.environ.get("SAMPLES", "3"))
    if not 1 <= samples <= 5:
        print("SAMPLES must be between 1 and 5.", file=sys.stderr)
        return 2

    rows: list[dict[str, str]] = []
    previous_request = 0.0
    for target_index, code in enumerate(codes, start=1):
        url = "https://discord.gg/" + code
        timings: dict[str, list[float]] = defaultdict(list)
        for sample in range(1, samples + 1):
            wait = 5.0 - (time.monotonic() - previous_request)
            if previous_request and wait > 0:
                time.sleep(wait)

            result = subprocess.run(
                [
                    "curl", "--location", "--silent", "--show-error",
                    "--connect-timeout", "3", "--max-time", "15",
                    "--output", os.devnull, "--write-out", WRITE_OUT, url,
                ],
                capture_output=True,
                text=True,
                check=False,
            )
            previous_request = time.monotonic()
            if result.returncode != 0:
                print(f"Target {target_index}, sample {sample}: curl failed ({result.returncode}); details suppressed.")
                continue

            parts = result.stdout.strip().split("\t")
            if len(parts) != len(FIELDS):
                print(f"Target {target_index}, sample {sample}: unexpected curl timing output.")
                continue

            http_code = parts[0]
            dns, connect, tls, ttfb, total = map(float, parts[1:6])
            row = {
                "target": str(target_index),
                "sample": str(sample),
                "http_code": http_code,
                "dns_ms": f"{dns * 1000:.3f}",
                "tcp_ms": f"{max(0.0, connect - dns) * 1000:.3f}",
                "tls_ms": f"{max(0.0, tls - connect) * 1000:.3f}",
                "ttfb_ms": f"{ttfb * 1000:.3f}",
                "total_ms": f"{total * 1000:.3f}",
                "download_bytes": parts[6],
            }
            rows.append(row)
            for key in ("dns_ms", "tcp_ms", "tls_ms", "ttfb_ms", "total_ms"):
                timings[key].append(float(row[key]))

            print(
                f"Target {target_index}/{len(codes)} sample {sample}/{samples}: "
                f"HTTP {http_code}, DNS {row['dns_ms']} ms, TCP {row['tcp_ms']} ms, "
                f"TLS {row['tls_ms']} ms, TTFB {row['ttfb_ms']} ms, "
                f"total {row['total_ms']} ms"
            )

            if http_code == "429":
                print("HTTP 429 received. Stopping immediately; no retries will be attempted.", file=sys.stderr)
                break

        if rows and rows[-1]["http_code"] == "429":
            break

    if rows:
        with OUTPUT.open("w", newline="", encoding="utf-8") as handle:
            writer = csv.DictWriter(handle, fieldnames=list(rows[0].keys()))
            writer.writeheader()
            writer.writerows(rows)

        print("\nPercentile summary (milliseconds; nearest-rank P95):")
        for key, label in (
            ("dns_ms", "DNS"),
            ("tcp_ms", "TCP"),
            ("tls_ms", "TLS"),
            ("ttfb_ms", "TTFB"),
            ("total_ms", "Total"),
        ):
            values = [float(row[key]) for row in rows]
            print(f"{label:>5}: P50 {median(values):.3f} | P95 {percentile(values, 0.95):.3f}")

    print(f"Recorded {len(rows)} samples across {len(codes)} configured targets.")
    return 1 if rows and rows[-1]["http_code"] == "429" else 0


if __name__ == "__main__":
    raise SystemExit(main())
