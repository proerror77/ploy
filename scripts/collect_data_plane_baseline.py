#!/usr/bin/env python3
"""
Collect a 24h (or custom duration) baseline from Prometheus text metrics.

Focus metrics:
- ploy_symbol_updates_total{source, symbol}
- ploy_source_messages_total{source}
- ploy_source_feed_health{source}
- ploy_source_subscriptions_total{source}
- ploy_broadcast_lag_total
- ploy_broadcast_drop_total

Outputs:
- <output_dir>/<run_id>.samples.jsonl
- <output_dir>/<run_id>.summary.json
- <output_dir>/<run_id>.baseline.json
- <output_dir>/<run_id>.symbol_rates.csv
- <output_dir>/<run_id>.source_rates.csv
"""

from __future__ import annotations

import argparse
import csv
import json
import math
import re
import statistics
import sys
import time
import urllib.error
import urllib.request
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import Dict, Iterable, List, Tuple


PROM_LINE_RE = re.compile(
    r"^([a-zA-Z_:][a-zA-Z0-9_:]*)(\{([^}]*)\})?\s+"
    r"([-+]?(?:\d+(?:\.\d*)?|\.\d+)(?:[eE][-+]?\d+)?)$"
)
LABEL_RE = re.compile(r'([a-zA-Z_][a-zA-Z0-9_]*)="((?:[^"\\]|\\.)*)"')

SYMBOL_COUNTER = "ploy_symbol_updates_total"
SOURCE_COUNTER = "ploy_source_messages_total"
SOURCE_HEALTH = "ploy_source_feed_health"
SOURCE_SUBS = "ploy_source_subscriptions_total"
BROADCAST_LAG = "ploy_broadcast_lag_total"
BROADCAST_DROP = "ploy_broadcast_drop_total"


@dataclass
class Snapshot:
    symbol_updates_total: Dict[str, float]
    source_messages_total: Dict[str, float]
    source_feed_health: Dict[str, float]
    source_subscriptions_total: Dict[str, float]
    broadcast_lag_total: float
    broadcast_drop_total: float


def utc_now_iso() -> str:
    return datetime.now(tz=timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def parse_labels(raw: str) -> Dict[str, str]:
    labels: Dict[str, str] = {}
    for match in LABEL_RE.finditer(raw):
        key = match.group(1)
        val = bytes(match.group(2), "utf-8").decode("unicode_escape")
        labels[key] = val
    return labels


def parse_prometheus_text(text: str) -> List[Tuple[str, Dict[str, str], float]]:
    rows: List[Tuple[str, Dict[str, str], float]] = []
    for line in text.splitlines():
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        match = PROM_LINE_RE.match(line)
        if not match:
            continue
        metric = match.group(1)
        labels_raw = match.group(3) or ""
        value = float(match.group(4))
        rows.append((metric, parse_labels(labels_raw), value))
    return rows


def extract_snapshot(rows: Iterable[Tuple[str, Dict[str, str], float]]) -> Snapshot:
    symbol_updates: Dict[str, float] = {}
    source_messages: Dict[str, float] = {}
    source_health: Dict[str, float] = {}
    source_subs: Dict[str, float] = {}
    lag_total = 0.0
    drop_total = 0.0

    for metric, labels, value in rows:
        if metric == SYMBOL_COUNTER:
            source = labels.get("source")
            symbol = labels.get("symbol")
            if source and symbol:
                symbol_updates[f"{source}|{symbol}"] = value
        elif metric == SOURCE_COUNTER:
            source = labels.get("source")
            if source:
                source_messages[source] = value
        elif metric == SOURCE_HEALTH:
            source = labels.get("source")
            if source:
                source_health[source] = value
        elif metric == SOURCE_SUBS:
            source = labels.get("source")
            if source:
                source_subs[source] = value
        elif metric == BROADCAST_LAG:
            lag_total = value
        elif metric == BROADCAST_DROP:
            drop_total = value

    return Snapshot(
        symbol_updates_total=symbol_updates,
        source_messages_total=source_messages,
        source_feed_health=source_health,
        source_subscriptions_total=source_subs,
        broadcast_lag_total=lag_total,
        broadcast_drop_total=drop_total,
    )


def counter_delta(prev: float | None, curr: float) -> float:
    if prev is None:
        return 0.0
    if curr >= prev:
        return curr - prev
    # Counter reset.
    return curr


def quantile(values: List[float], q: float) -> float:
    if not values:
        return 0.0
    if len(values) == 1:
        return values[0]
    vals = sorted(values)
    pos = (len(vals) - 1) * q
    low = int(math.floor(pos))
    high = int(math.ceil(pos))
    if low == high:
        return vals[low]
    frac = pos - low
    return vals[low] * (1.0 - frac) + vals[high] * frac


def fetch_metrics(url: str, timeout_secs: float) -> str:
    req = urllib.request.Request(url=url, headers={"User-Agent": "ploy-baseline-collector/1.0"})
    with urllib.request.urlopen(req, timeout=timeout_secs) as resp:
        return resp.read().decode("utf-8", errors="replace")


def summarize_rate_series(
    deltas_by_key: Dict[str, List[float]],
    interval_secs: float,
    elapsed_secs: float,
) -> Dict[str, Dict[str, float]]:
    out: Dict[str, Dict[str, float]] = {}
    for key, deltas in deltas_by_key.items():
        total = float(sum(deltas))
        rates = [d / interval_secs for d in deltas] if interval_secs > 0 else [0.0 for _ in deltas]
        avg = total / elapsed_secs if elapsed_secs > 0 else 0.0
        out[key] = {
            "total_messages": total,
            "avg_rps": avg,
            "min_rps": min(rates) if rates else 0.0,
            "max_rps": max(rates) if rates else 0.0,
            "p50_rps": quantile(rates, 0.50),
            "p95_rps": quantile(rates, 0.95),
            "samples": float(len(rates)),
        }
    return out


def write_csv(path: Path, headers: List[str], rows: List[Dict[str, object]]) -> None:
    with path.open("w", newline="", encoding="utf-8") as f:
        writer = csv.DictWriter(f, fieldnames=headers)
        writer.writeheader()
        for row in rows:
            writer.writerow(row)


def main() -> int:
    parser = argparse.ArgumentParser(description="Collect data-plane baseline from /metrics")
    parser.add_argument(
        "--metrics-url",
        default="http://127.0.0.1:9090/metrics",
        help="Prometheus text endpoint URL",
    )
    parser.add_argument("--duration-secs", type=int, default=24 * 60 * 60)
    parser.add_argument("--interval-secs", type=int, default=10)
    parser.add_argument("--timeout-secs", type=float, default=5.0)
    parser.add_argument("--output-dir", default="data/baseline")
    parser.add_argument(
        "--run-id",
        default=datetime.now(tz=timezone.utc).strftime("baseline-%Y%m%d-%H%M%S"),
    )
    parser.add_argument(
        "--rollback-drop-pct",
        type=float,
        default=0.05,
        help="Drop threshold used to generate baseline min_allowed_rps",
    )
    parser.add_argument(
        "--rollback-sustain-secs",
        type=int,
        default=60,
        help="Sustain window used to generate baseline rollback rule",
    )
    args = parser.parse_args()

    if args.interval_secs <= 0:
        print("--interval-secs must be > 0", file=sys.stderr)
        return 2
    if args.duration_secs <= 0:
        print("--duration-secs must be > 0", file=sys.stderr)
        return 2

    out_dir = Path(args.output_dir)
    out_dir.mkdir(parents=True, exist_ok=True)

    samples_path = out_dir / f"{args.run_id}.samples.jsonl"
    summary_path = out_dir / f"{args.run_id}.summary.json"
    baseline_path = out_dir / f"{args.run_id}.baseline.json"
    symbol_csv_path = out_dir / f"{args.run_id}.symbol_rates.csv"
    source_csv_path = out_dir / f"{args.run_id}.source_rates.csv"

    prev_snapshot: Snapshot | None = None
    symbol_deltas: Dict[str, List[float]] = {}
    source_deltas: Dict[str, List[float]] = {}
    lag_deltas: List[float] = []
    drop_deltas: List[float] = []

    start_epoch = time.time()
    end_epoch = start_epoch + args.duration_secs
    sample_count = 0
    error_count = 0

    print(
        f"[{utc_now_iso()}] starting baseline collection: "
        f"url={args.metrics_url} duration={args.duration_secs}s interval={args.interval_secs}s"
    )
    print(f"[{utc_now_iso()}] writing samples to: {samples_path}")

    with samples_path.open("w", encoding="utf-8") as out:
        while time.time() < end_epoch:
            now_epoch = time.time()
            now_iso = datetime.fromtimestamp(now_epoch, tz=timezone.utc).strftime(
                "%Y-%m-%dT%H:%M:%SZ"
            )

            try:
                text = fetch_metrics(args.metrics_url, args.timeout_secs)
                rows = parse_prometheus_text(text)
                snap = extract_snapshot(rows)
                sample_count += 1
            except (urllib.error.URLError, TimeoutError, OSError, ValueError) as exc:
                error_count += 1
                out.write(
                    json.dumps(
                        {
                            "ts": now_iso,
                            "epoch_s": now_epoch,
                            "error": str(exc),
                        },
                        ensure_ascii=True,
                    )
                    + "\n"
                )
                out.flush()
                time.sleep(args.interval_secs)
                continue

            symbol_rates: Dict[str, float] = {}
            source_rates: Dict[str, float] = {}
            lag_delta = 0.0
            drop_delta = 0.0

            if prev_snapshot is not None:
                symbol_keys = set(prev_snapshot.symbol_updates_total.keys()) | set(
                    snap.symbol_updates_total.keys()
                )
                for key in symbol_keys:
                    d = counter_delta(
                        prev_snapshot.symbol_updates_total.get(key), snap.symbol_updates_total.get(key, 0.0)
                    )
                    symbol_deltas.setdefault(key, []).append(d)
                    symbol_rates[key] = d / args.interval_secs

                source_keys = set(prev_snapshot.source_messages_total.keys()) | set(
                    snap.source_messages_total.keys()
                )
                for key in source_keys:
                    d = counter_delta(
                        prev_snapshot.source_messages_total.get(key), snap.source_messages_total.get(key, 0.0)
                    )
                    source_deltas.setdefault(key, []).append(d)
                    source_rates[key] = d / args.interval_secs

                lag_delta = counter_delta(prev_snapshot.broadcast_lag_total, snap.broadcast_lag_total)
                drop_delta = counter_delta(prev_snapshot.broadcast_drop_total, snap.broadcast_drop_total)
                lag_deltas.append(lag_delta)
                drop_deltas.append(drop_delta)

            prev_snapshot = snap

            out.write(
                json.dumps(
                    {
                        "ts": now_iso,
                        "epoch_s": now_epoch,
                        "symbol_updates_total": snap.symbol_updates_total,
                        "source_messages_total": snap.source_messages_total,
                        "source_feed_health": snap.source_feed_health,
                        "source_subscriptions_total": snap.source_subscriptions_total,
                        "broadcast_lag_total": snap.broadcast_lag_total,
                        "broadcast_drop_total": snap.broadcast_drop_total,
                        "symbol_update_rps": symbol_rates,
                        "source_message_rps": source_rates,
                        "broadcast_lag_delta": lag_delta,
                        "broadcast_drop_delta": drop_delta,
                    },
                    ensure_ascii=True,
                    separators=(",", ":"),
                )
                + "\n"
            )
            out.flush()

            # Lightweight progress line every ~1 minute.
            if sample_count == 1 or sample_count % max(1, int(60 / args.interval_secs)) == 0:
                print(
                    f"[{utc_now_iso()}] samples={sample_count} errors={error_count} "
                    f"symbols={len(snap.symbol_updates_total)} sources={len(snap.source_messages_total)}"
                )

            sleep_for = max(0.0, args.interval_secs - (time.time() - now_epoch))
            time.sleep(sleep_for)

    elapsed = max(1.0, time.time() - start_epoch)
    symbol_summary = summarize_rate_series(symbol_deltas, args.interval_secs, elapsed)
    source_summary = summarize_rate_series(source_deltas, args.interval_secs, elapsed)

    summary = {
        "run_id": args.run_id,
        "generated_at": utc_now_iso(),
        "metrics_url": args.metrics_url,
        "duration_secs_requested": args.duration_secs,
        "duration_secs_observed": elapsed,
        "interval_secs": args.interval_secs,
        "sample_count": sample_count,
        "error_count": error_count,
        "symbol_rates": symbol_summary,
        "source_rates": source_summary,
        "broadcast": {
            "lag_total": float(sum(lag_deltas)),
            "drop_total": float(sum(drop_deltas)),
            "lag_avg_per_interval": (sum(lag_deltas) / len(lag_deltas)) if lag_deltas else 0.0,
            "drop_avg_per_interval": (sum(drop_deltas) / len(drop_deltas)) if drop_deltas else 0.0,
        },
    }
    summary_path.write_text(json.dumps(summary, indent=2, sort_keys=True), encoding="utf-8")

    symbol_rows: List[Dict[str, object]] = []
    for key, stats in sorted(symbol_summary.items()):
        source, symbol = key.split("|", 1)
        symbol_rows.append(
            {
                "source": source,
                "symbol": symbol,
                **stats,
            }
        )

    source_rows: List[Dict[str, object]] = []
    for source, stats in sorted(source_summary.items()):
        source_rows.append({"source": source, **stats})

    write_csv(
        symbol_csv_path,
        ["source", "symbol", "total_messages", "avg_rps", "min_rps", "max_rps", "p50_rps", "p95_rps", "samples"],
        symbol_rows,
    )
    write_csv(
        source_csv_path,
        ["source", "total_messages", "avg_rps", "min_rps", "max_rps", "p50_rps", "p95_rps", "samples"],
        source_rows,
    )

    baseline = {
        "run_id": args.run_id,
        "generated_at": utc_now_iso(),
        "interval_secs": args.interval_secs,
        "rollback_rule": {
            "drop_pct": args.rollback_drop_pct,
            "sustain_secs": args.rollback_sustain_secs,
        },
        "series": {
            "symbol_updates": [],
            "source_messages": [],
        },
    }

    for row in symbol_rows:
        baseline_rps = row["p50_rps"] if row["p50_rps"] > 0 else row["avg_rps"]
        baseline["series"]["symbol_updates"].append(
            {
                "key": f"{row['source']}|{row['symbol']}",
                "source": row["source"],
                "symbol": row["symbol"],
                "baseline_rps": baseline_rps,
                "min_allowed_rps": baseline_rps * (1.0 - args.rollback_drop_pct),
            }
        )
    for row in source_rows:
        baseline_rps = row["p50_rps"] if row["p50_rps"] > 0 else row["avg_rps"]
        baseline["series"]["source_messages"].append(
            {
                "key": row["source"],
                "source": row["source"],
                "baseline_rps": baseline_rps,
                "min_allowed_rps": baseline_rps * (1.0 - args.rollback_drop_pct),
            }
        )

    baseline_path.write_text(json.dumps(baseline, indent=2, sort_keys=True), encoding="utf-8")

    print(f"[{utc_now_iso()}] done: samples={sample_count} errors={error_count}")
    print(f"  samples:  {samples_path}")
    print(f"  summary:  {summary_path}")
    print(f"  baseline: {baseline_path}")
    print(f"  csv(sym): {symbol_csv_path}")
    print(f"  csv(src): {source_csv_path}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
