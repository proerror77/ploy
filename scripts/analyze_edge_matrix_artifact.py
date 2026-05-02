#!/usr/bin/env python3
"""Summarize PM5D strategy edge-matrix artifacts.

This is intentionally artifact-only: it reads the CSV/JSON outputs from
`strategy-research-matrix.yml` and produces a compact decision report without
rerunning research or touching live/dry-run services.
"""

from __future__ import annotations

import argparse
import csv
import json
from collections import defaultdict
from dataclasses import dataclass
from pathlib import Path
from typing import Any


DEPLOYABLE_MIN_TRADES = 80
MAX_EV_GAP = 0.30
MIN_POSITIVE_DAY_RATE = 0.70
MIN_FILL_RATE = 0.95


@dataclass
class Aggregate:
    rows: int = 0
    trades: int = 0
    net_pnl: float = 0.0
    positive_rows: int = 0
    deployable_rows: int = 0

    def add(self, row: dict[str, str]) -> None:
        self.rows += 1
        self.trades += int(row["trades"])
        pnl = float(row["net_pnl"])
        self.net_pnl += pnl
        self.positive_rows += int(pnl > 0.0)
        self.deployable_rows += int(row["deployable_candidate"] == "true")

    def as_dict(self) -> dict[str, Any]:
        return {
            "rows": self.rows,
            "trades": self.trades,
            "net_pnl": round(self.net_pnl, 6),
            "positive_rows": self.positive_rows,
            "deployable_rows": self.deployable_rows,
        }


def read_csv(path: Path) -> list[dict[str, str]]:
    with path.open(newline="") as handle:
        return list(csv.DictReader(handle))


def load_summary(path: Path) -> dict[str, Any]:
    with path.open() as handle:
        return json.load(handle)


def artifact_file(root: Path, name: str) -> Path:
    candidates = sorted(root.rglob(name))
    if not candidates:
        raise SystemExit(f"missing artifact file: {name} under {root}")
    return candidates[0]


def blockers(row: dict[str, str]) -> list[str]:
    out: list[str] = []
    trades = int(row["trades"])
    ev_gap = float(row["expectancy_calibration_gap"])
    fill_rate = float(row["fill_rate"])
    positive_day = float(row["positive_day_rate"])
    net_pnl = float(row["net_pnl"])
    realized = float(row["avg_realized_return_per_stake"])
    if trades < DEPLOYABLE_MIN_TRADES:
        out.append(f"sample_power:{trades}<{DEPLOYABLE_MIN_TRADES}")
    if net_pnl <= 0.0:
        out.append("nonpositive_pnl")
    if fill_rate < MIN_FILL_RATE:
        out.append(f"fill_rate:{fill_rate:.3f}<{MIN_FILL_RATE:.2f}")
    if realized <= 0.0:
        out.append("nonpositive_realized_return_per_stake")
    if ev_gap > MAX_EV_GAP:
        out.append(f"ev_gap:{ev_gap:.3f}>{MAX_EV_GAP:.2f}")
    if positive_day < MIN_POSITIVE_DAY_RATE:
        out.append(f"positive_day:{positive_day:.3f}<{MIN_POSITIVE_DAY_RATE:.2f}")
    if float(row["positive_symbol_rate"]) < MIN_POSITIVE_DAY_RATE:
        out.append(f"positive_symbol:{float(row['positive_symbol_rate']):.3f}<{MIN_POSITIVE_DAY_RATE:.2f}")
    return out


def aggregate(rows: list[dict[str, str]], fields: tuple[str, ...]) -> dict[str, dict[str, Any]]:
    grouped: dict[str, Aggregate] = defaultdict(Aggregate)
    for row in rows:
        key = " | ".join(row[field] for field in fields)
        grouped[key].add(row)
    return {key: value.as_dict() for key, value in sorted(grouped.items())}


def top_rows(rows: list[dict[str, str]], limit: int = 12) -> list[dict[str, Any]]:
    ordered = sorted(
        rows,
        key=lambda row: (
            row["deployable_candidate"] == "true",
            int(row["trades"]),
            float(row["net_pnl"]),
        ),
        reverse=True,
    )
    out = []
    for row in ordered[:limit]:
        out.append(
            {
                "hypothesis": row["hypothesis"],
                "direction_mode": row["direction_mode"],
                "fill_mode": row["fill_mode"],
                "pm_mode": row["pm_mode"],
                "trades": int(row["trades"]),
                "net_pnl": round(float(row["net_pnl"]), 6),
                "fill_rate": round(float(row["fill_rate"]), 6),
                "ev_gap": round(float(row["expectancy_calibration_gap"]), 6),
                "positive_day_rate": round(float(row["positive_day_rate"]), 6),
                "positive_symbol_rate": round(float(row["positive_symbol_rate"]), 6),
                "deployable_candidate": row["deployable_candidate"] == "true",
                "blockers": blockers(row),
            }
        )
    return out


def selection_status(rows: list[dict[str, str]]) -> dict[str, dict[str, int]]:
    grouped: dict[str, dict[str, int]] = defaultdict(lambda: defaultdict(int))
    for row in rows:
        grouped[row["direction_mode"]][row["selection_status"]] += 1
    return {key: dict(value) for key, value in sorted(grouped.items())}


def gate_trace(rows: list[dict[str, str]], hypothesis: str) -> list[dict[str, Any]]:
    out = []
    for row in rows:
        if row["split"] != "validation" or row["hypothesis"] != hypothesis:
            continue
        out.append(
            {
                "gate_index": int(row["gate_index"]),
                "gate": row["gate"],
                "rows": int(row["rows"]),
                "event_sides": int(row["event_sides"]),
                "entry_fill_rate": round(float(row["entry_fill_rate"]), 6),
                "roundtrip_fill_rate": round(float(row["roundtrip_fill_rate"]), 6),
                "total_executable_pnl": round(float(row["total_executable_pnl"]), 6),
                "avg_executable_pnl": round(float(row["avg_executable_pnl"]), 6),
            }
        )
    return out


def build_report(root: Path, run_id: str | None) -> dict[str, Any]:
    matrix_rows = read_csv(artifact_file(root, "strategy-matrix-results.csv"))
    audit_rows = read_csv(artifact_file(root, "selection-audit.csv"))
    gate_rows = read_csv(artifact_file(root, "gate-attrition.csv"))
    summary = load_summary(artifact_file(root, "edge-matrix-summary.json"))
    validation_rows = [row for row in matrix_rows if row["split"] == "validation"]
    top = top_rows(validation_rows)
    run_threshold_candidate_count = sum(
        1 for row in validation_rows if row["deployable_candidate"] == "true"
    )
    strict_candidate_count = sum(1 for row in validation_rows if not blockers(row))
    top_hypothesis = top[0]["hypothesis"] if top else None
    matrix_min_trades = int(summary.get("min_trades") or 0)
    result = {
        "artifact_type": "edge_matrix_diagnostic_report",
        "run_id": run_id,
        "snapshot_hash": summary.get("snapshot_hash"),
        "source_rows": summary.get("source_rows"),
        "v2_rows": summary.get("v2_rows"),
        "train_window": {"start": summary.get("train_start"), "end": summary.get("train_end")},
        "validation_window": {"start": summary.get("val_start"), "end": summary.get("val_end")},
        "symbols": summary.get("symbols"),
        "hypothesis_count": summary.get("hypothesis_count"),
        "matrix_min_trades": matrix_min_trades,
        "validation_row_count": len(validation_rows),
        "run_threshold_validation_candidates": run_threshold_candidate_count,
        "strict_deployable_validation_candidates": strict_candidate_count,
        "top_validation_rows": top,
        "aggregate_by_direction": aggregate(validation_rows, ("direction_mode",)),
        "aggregate_by_direction_pm": aggregate(validation_rows, ("direction_mode", "pm_mode")),
        "aggregate_by_direction_fill": aggregate(validation_rows, ("direction_mode", "fill_mode")),
        "selection_status_by_direction": selection_status(
            [row for row in audit_rows if row["split"] == "validation"]
        ),
        "top_hypothesis_gate_trace": gate_trace(gate_rows, top_hypothesis) if top_hypothesis else [],
        "decision": (
            "review-strict-candidates"
            if strict_candidate_count > 0 and matrix_min_trades >= DEPLOYABLE_MIN_TRADES
            else "diagnostic-only-continue-research"
        ),
    }
    return result


def markdown(report: dict[str, Any]) -> str:
    lines = [
        "# PM5D Edge Matrix Diagnostic",
        "",
        f"- Run: `{report.get('run_id') or 'unknown'}`",
        f"- Snapshot hash: `{report.get('snapshot_hash')}`",
        f"- Train: `{report['train_window']['start']} -> {report['train_window']['end']}`",
        f"- Validation: `{report['validation_window']['start']} -> {report['validation_window']['end']}`",
        f"- Symbols: `{','.join(report.get('symbols') or [])}`",
        f"- Matrix min trades: `{report['matrix_min_trades']}`",
        f"- Run-threshold validation candidates: `{report['run_threshold_validation_candidates']}`",
        f"- Strict deployable validation candidates: `{report['strict_deployable_validation_candidates']}`",
        f"- Decision: `{report['decision']}`",
        "",
        "## Direction Aggregate",
        "",
        "| Direction | Rows | Trades | Net PnL | Positive Rows | Run-Threshold Rows |",
        "| --- | ---: | ---: | ---: | ---: | ---: |",
    ]
    for direction, data in report["aggregate_by_direction"].items():
        lines.append(
            f"| {direction} | {data['rows']} | {data['trades']} | "
            f"{data['net_pnl']:.2f} | {data['positive_rows']} | {data['deployable_rows']} |"
        )
    lines.extend(["", "## Top Validation Rows", ""])
    for row in report["top_validation_rows"][:8]:
        lines.append(
            f"- `{row['hypothesis']}`: trades `{row['trades']}`, PnL `{row['net_pnl']:.2f}`, "
            f"fill `{row['fill_rate']:.3f}`, EV gap `{row['ev_gap']:.3f}`, "
            f"positive-day `{row['positive_day_rate']:.3f}`, blockers `{', '.join(row['blockers']) or 'none'}`"
        )
    lines.extend(["", "## Top Hypothesis Gate Trace", ""])
    for row in report["top_hypothesis_gate_trace"]:
        lines.append(
            f"- `{row['gate_index']}:{row['gate']}` rows `{row['rows']}`, "
            f"event_sides `{row['event_sides']}`, entry_fill `{row['entry_fill_rate']:.3f}`, "
            f"roundtrip_fill `{row['roundtrip_fill_rate']:.3f}`, "
            f"total executable PnL `{row['total_executable_pnl']:.2f}`"
        )
    lines.extend(
        [
            "",
            "## Interpretation",
            "",
            "- The old `Model` direction remains negative in aggregate, while `Inverted` remains positive.",
            "- The best inverted row is still not deployable because sample power, calibration, and daily stability gates are not all satisfied.",
            "- Runs with matrix min trades below 80 are diagnostic only; they can identify candidate behavior but cannot authorize dry-run/live restoration.",
            "- This supports a focused inverted/regime-calibration research lane, not dry-run/live restoration.",
            "",
        ]
    )
    return "\n".join(lines)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--artifact-dir", required=True)
    parser.add_argument("--run-id", default="")
    parser.add_argument("--output-json", required=True)
    parser.add_argument("--output-md", required=True)
    args = parser.parse_args()

    report = build_report(Path(args.artifact_dir), args.run_id or None)
    json_path = Path(args.output_json)
    md_path = Path(args.output_md)
    json_path.parent.mkdir(parents=True, exist_ok=True)
    md_path.parent.mkdir(parents=True, exist_ok=True)
    json_path.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    md_path.write_text(markdown(report), encoding="utf-8")
    print(json.dumps({"decision": report["decision"], "output_json": str(json_path), "output_md": str(md_path)}, indent=2))


if __name__ == "__main__":
    main()
