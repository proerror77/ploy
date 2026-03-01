#!/usr/bin/env python3
"""
Validate drop-rate rollback criteria against collected baseline samples.

Rollback default rule:
- observed rate < (baseline_rate * 0.95)
- sustained for >= 60 seconds
"""

from __future__ import annotations

import argparse
import json
import math
import statistics
import sys
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import Dict, Iterable, List, Tuple


def utc_now_iso() -> str:
    return datetime.now(tz=timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def load_json(path: Path) -> dict:
    return json.loads(path.read_text(encoding="utf-8"))


def load_jsonl(path: Path) -> List[dict]:
    rows: List[dict] = []
    with path.open("r", encoding="utf-8") as f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            try:
                rows.append(json.loads(line))
            except json.JSONDecodeError:
                continue
    return rows


def infer_interval_secs(samples: List[dict], fallback: float) -> float:
    epochs = [float(s["epoch_s"]) for s in samples if "epoch_s" in s]
    if len(epochs) < 2:
        return fallback
    deltas = [b - a for a, b in zip(epochs, epochs[1:]) if b > a]
    if not deltas:
        return fallback
    return statistics.median(deltas)


@dataclass
class Breach:
    scope: str
    key: str
    baseline_rps: float
    min_allowed_rps: float
    max_consecutive_breach_secs: float
    first_breach_ts: str | None


def evaluate_series(
    samples: List[dict],
    rates_field: str,
    baseline_entries: Iterable[dict],
    interval_secs: float,
    sustain_secs: float,
    drop_pct: float,
    scope: str,
) -> Tuple[List[Breach], List[dict]]:
    breaches: List[Breach] = []
    details: List[dict] = []

    for entry in baseline_entries:
        key = str(entry["key"])
        baseline_rps = float(entry.get("baseline_rps", 0.0))
        if baseline_rps <= 0:
            continue
        min_allowed = baseline_rps * (1.0 - drop_pct)

        consecutive = 0.0
        max_consecutive = 0.0
        first_breach_ts: str | None = None
        latest_rate = 0.0

        for sample in samples:
            rates = sample.get(rates_field)
            if not isinstance(rates, dict):
                continue
            rate = float(rates.get(key, 0.0))
            latest_rate = rate
            if rate < min_allowed:
                consecutive += interval_secs
                if first_breach_ts is None:
                    first_breach_ts = sample.get("ts")
                if consecutive > max_consecutive:
                    max_consecutive = consecutive
            else:
                consecutive = 0.0
                first_breach_ts = None

        details.append(
            {
                "scope": scope,
                "key": key,
                "baseline_rps": baseline_rps,
                "min_allowed_rps": min_allowed,
                "latest_rps": latest_rate,
                "max_consecutive_breach_secs": max_consecutive,
            }
        )

        if max_consecutive >= sustain_secs:
            breaches.append(
                Breach(
                    scope=scope,
                    key=key,
                    baseline_rps=baseline_rps,
                    min_allowed_rps=min_allowed,
                    max_consecutive_breach_secs=max_consecutive,
                    first_breach_ts=first_breach_ts,
                )
            )

    return breaches, details


def main() -> int:
    parser = argparse.ArgumentParser(description="Validate rollback drop-rate criteria")
    parser.add_argument("--baseline-json", required=True)
    parser.add_argument("--samples-jsonl", required=True)
    parser.add_argument(
        "--scope",
        choices=["symbol", "source", "both"],
        default="both",
        help="Validate symbol updates, source messages, or both",
    )
    parser.add_argument("--drop-pct", type=float, default=None)
    parser.add_argument("--sustain-secs", type=float, default=None)
    parser.add_argument("--output-json", default=None)
    args = parser.parse_args()

    baseline_path = Path(args.baseline_json)
    samples_path = Path(args.samples_jsonl)

    baseline = load_json(baseline_path)
    samples = [s for s in load_jsonl(samples_path) if "error" not in s]
    if not samples:
        print("no usable samples (all errored or empty)", file=sys.stderr)
        return 2

    rule = baseline.get("rollback_rule", {})
    drop_pct = float(args.drop_pct if args.drop_pct is not None else rule.get("drop_pct", 0.05))
    sustain_secs = float(
        args.sustain_secs if args.sustain_secs is not None else rule.get("sustain_secs", 60)
    )
    interval_secs = infer_interval_secs(samples, float(baseline.get("interval_secs", 10)))

    all_breaches: List[Breach] = []
    details: List[dict] = []
    series = baseline.get("series", {})

    if args.scope in ("symbol", "both"):
        b, d = evaluate_series(
            samples=samples,
            rates_field="symbol_update_rps",
            baseline_entries=series.get("symbol_updates", []),
            interval_secs=interval_secs,
            sustain_secs=sustain_secs,
            drop_pct=drop_pct,
            scope="symbol",
        )
        all_breaches.extend(b)
        details.extend(d)

    if args.scope in ("source", "both"):
        b, d = evaluate_series(
            samples=samples,
            rates_field="source_message_rps",
            baseline_entries=series.get("source_messages", []),
            interval_secs=interval_secs,
            sustain_secs=sustain_secs,
            drop_pct=drop_pct,
            scope="source",
        )
        all_breaches.extend(b)
        details.extend(d)

    report = {
        "generated_at": utc_now_iso(),
        "baseline_json": str(baseline_path),
        "samples_jsonl": str(samples_path),
        "drop_pct": drop_pct,
        "sustain_secs": sustain_secs,
        "interval_secs": interval_secs,
        "sample_count": len(samples),
        "breach_count": len(all_breaches),
        "breaches": [
            {
                "scope": b.scope,
                "key": b.key,
                "baseline_rps": b.baseline_rps,
                "min_allowed_rps": b.min_allowed_rps,
                "max_consecutive_breach_secs": b.max_consecutive_breach_secs,
                "first_breach_ts": b.first_breach_ts,
            }
            for b in all_breaches
        ],
        "series_details": details,
    }

    if args.output_json:
        Path(args.output_json).write_text(
            json.dumps(report, indent=2, sort_keys=True), encoding="utf-8"
        )

    if all_breaches:
        print("ROLLBACK_CONDITION_TRIGGERED")
        for b in all_breaches:
            print(
                f"- [{b.scope}] {b.key}: baseline={b.baseline_rps:.4f} "
                f"min_allowed={b.min_allowed_rps:.4f} "
                f"max_breach={b.max_consecutive_breach_secs:.1f}s"
            )
        return 1

    print(
        f"OK: no sustained drop-rate breach "
        f"(drop_pct={drop_pct:.3f}, sustain_secs={sustain_secs:.0f}, samples={len(samples)})"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
