#!/usr/bin/env python3
"""Analyze PM5D dry-run correction matrix artifacts.

This is an artifact-only attribution pass. It does not rerun research and does
not touch dry-run/live services. The goal is to separate real candidate
interactions from misleading marginal slices such as "UP-only looked good".
"""

from __future__ import annotations

import argparse
import csv
import json
import re
from collections import Counter, defaultdict
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Iterable


DECISION_RANK = {"deployable_candidate": 3, "watchlist": 2, "reject": 1}


@dataclass
class Aggregate:
    rows: int = 0
    trades: int = 0
    net_pnl: float = 0.0
    positive_rows: int = 0
    deployable_rows: int = 0
    watchlist_rows: int = 0

    def add_result(self, row: dict[str, str]) -> None:
        pnl = as_float(row["net_pnl"])
        self.rows += 1
        self.trades += as_int(row["trades"])
        self.net_pnl += pnl
        self.positive_rows += int(pnl > 0.0)
        self.deployable_rows += int(row.get("deployable_candidate") == "true")

    def add_paired(self, row: dict[str, str]) -> None:
        pnl = as_float(row["val_net_pnl"])
        self.rows += 1
        self.trades += as_int(row["val_trades"])
        self.net_pnl += pnl
        self.positive_rows += int(pnl > 0.0)
        self.deployable_rows += int(row.get("decision") == "deployable_candidate")
        self.watchlist_rows += int(row.get("decision") == "watchlist")

    def as_dict(self) -> dict[str, Any]:
        return {
            "rows": self.rows,
            "trades": self.trades,
            "net_pnl": round(self.net_pnl, 6),
            "avg_pnl_per_trade": round(self.net_pnl / self.trades, 6) if self.trades else None,
            "positive_row_rate": round(self.positive_rows / self.rows, 6) if self.rows else None,
            "deployable_rows": self.deployable_rows,
            "watchlist_rows": self.watchlist_rows,
        }


def as_float(raw: str) -> float:
    return float(raw) if raw not in ("", "nan", "NaN") else float("nan")


def as_int(raw: str) -> int:
    return int(float(raw))


def read_csv(path: Path) -> list[dict[str, str]]:
    with path.open(newline="") as handle:
        return list(csv.DictReader(handle))


def read_json(path: Path) -> dict[str, Any]:
    with path.open() as handle:
        return json.load(handle)


def artifact_file(root: Path, name: str) -> Path:
    candidates = sorted(root.rglob(name))
    if not candidates:
        raise SystemExit(f"missing artifact file: {name} under {root}")
    return candidates[0]


def aggregate_results(rows: Iterable[dict[str, str]], fields: tuple[str, ...]) -> dict[str, Any]:
    grouped: dict[str, Aggregate] = defaultdict(Aggregate)
    for row in rows:
        key = " | ".join(row[field] for field in fields)
        grouped[key].add_result(row)
    return {
        key: value.as_dict()
        for key, value in sorted(grouped.items(), key=lambda item: item[1].net_pnl, reverse=True)
    }


def aggregate_paired(rows: Iterable[dict[str, str]], fields: tuple[str, ...]) -> dict[str, Any]:
    grouped: dict[str, Aggregate] = defaultdict(Aggregate)
    for row in rows:
        key = " | ".join(row[field] for field in fields)
        grouped[key].add_paired(row)
    return {
        key: value.as_dict()
        for key, value in sorted(grouped.items(), key=lambda item: item[1].net_pnl, reverse=True)
    }


def parse_hypothesis(hypothesis: str) -> dict[str, Any]:
    ttr = re.search(r"_ttr(?P<min>\d+)_(?P<max>\d+)_", hypothesis)
    price = re.search(r"_px(?P<min>\d+)_(?P<max>\d+)_", hypothesis)
    ev = re.search(r"_ev(?P<ev>\d+\.\d+)_", hypothesis)
    return {
        "ttr_bucket": f"{ttr.group('min')}-{ttr.group('max')}" if ttr else "unknown",
        "price_bucket": (
            f"{int(price.group('min')) / 100:.2f}-{int(price.group('max')) / 100:.2f}"
            if price
            else "unknown"
        ),
        "ev_floor": ev.group("ev") if ev else "unknown",
    }


def enriched_result_rows(rows: list[dict[str, str]]) -> list[dict[str, str]]:
    out: list[dict[str, str]] = []
    for row in rows:
        enriched = dict(row)
        for key, value in parse_hypothesis(row["hypothesis"]).items():
            enriched[key] = str(value)
        out.append(enriched)
    return out


def top_paired(rows: list[dict[str, str]], limit: int = 15) -> list[dict[str, Any]]:
    ordered = sorted(
        rows,
        key=lambda row: (
            DECISION_RANK.get(row["decision"], 0),
            as_float(row["val_net_pnl"]),
            as_int(row["val_trades"]),
            as_float(row["train_net_pnl"]),
        ),
        reverse=True,
    )
    return [
        {
            "hypothesis": row["hypothesis"],
            "decision": row["decision"],
            "direction_mode": row["direction_mode"],
            "side_policy": row["side_policy"],
            "fill_mode": row["fill_mode"],
            "pm_mode": row["pm_mode"],
            "window_policy": row["window_policy"],
            "liquidity_policy": row["liquidity_policy"],
            "train_trades": as_int(row["train_trades"]),
            "train_net_pnl": round(as_float(row["train_net_pnl"]), 6),
            "val_trades": as_int(row["val_trades"]),
            "val_net_pnl": round(as_float(row["val_net_pnl"]), 6),
            "val_fill_rate": round(as_float(row["val_fill_rate"]), 6),
            "val_ev_gap": round(as_float(row["val_expectancy_calibration_gap"]), 6),
            "reason": row["reason"],
        }
        for row in ordered[:limit]
    ]


def support_equivalence(validation_rows: list[dict[str, str]]) -> list[dict[str, Any]]:
    dimensions = [
        "direction_mode",
        "fill_mode",
        "pm_mode",
        "window_policy",
        "liquidity_policy",
        "ttr_bucket",
        "price_bucket",
        "ev_floor",
    ]
    grouped: dict[tuple[str, ...], dict[str, tuple[int, float]]] = defaultdict(dict)
    for row in validation_rows:
        key = tuple(row[field] for field in dimensions)
        grouped[key][row["side_policy"]] = (as_int(row["trades"]), round(as_float(row["net_pnl"]), 6))

    findings: list[dict[str, Any]] = []
    for key, sides in grouped.items():
        both = sides.get("Both")
        up = sides.get("UpOnly")
        down = sides.get("DownOnly")
        if both and up and both == up and (not down or down == (0, 0.0)):
            findings.append(
                {
                    "kind": "both_equals_up_only",
                    "context": dict(zip(dimensions, key)),
                    "trades": both[0],
                    "net_pnl": both[1],
                }
            )
        if both and down and both == down and (not up or up == (0, 0.0)):
            findings.append(
                {
                    "kind": "both_equals_down_only",
                    "context": dict(zip(dimensions, key)),
                    "trades": both[0],
                    "net_pnl": both[1],
                }
            )
    findings.sort(key=lambda row: (row["trades"], abs(row["net_pnl"])), reverse=True)
    return findings[:20]


def blocker_counts(rows: list[dict[str, str]]) -> dict[str, int]:
    counts: Counter[str] = Counter()
    for row in rows:
        for reason in row["reason"].split("|"):
            if reason:
                counts[reason] += 1
    return dict(counts.most_common())


def build_report(root: Path, run_id: str | None) -> dict[str, Any]:
    paired = read_csv(artifact_file(root, "dryrun-correction-paired-candidates.csv"))
    results = enriched_result_rows(read_csv(artifact_file(root, "dryrun-correction-results.csv")))
    gate_rows = read_csv(artifact_file(root, "gate-attrition.csv"))
    summary = read_json(artifact_file(root, "dryrun-correction-summary.json"))
    validation = [row for row in results if row["split"] == "validation"]
    watchlist = [row for row in paired if row["decision"] == "watchlist"]

    decision_counts = Counter(row["decision"] for row in paired)
    direction_side = aggregate_results(validation, ("direction_mode", "side_policy"))
    direction_side_fill = aggregate_results(validation, ("direction_mode", "side_policy", "fill_mode"))
    regime = aggregate_results(
        validation,
        ("direction_mode", "side_policy", "fill_mode", "pm_mode", "ttr_bucket", "price_bucket"),
    )

    result = {
        "artifact_type": "dryrun_correction_attribution",
        "run_id": run_id,
        "snapshot_hash": summary.get("snapshot_hash"),
        "hypothesis_count": summary.get("hypothesis_count"),
        "selection_audit_enabled": summary.get("selection_audit_enabled"),
        "v2_rows": summary.get("v2_rows"),
        "min_trades": summary.get("min_trades"),
        "decision_counts": dict(decision_counts),
        "watchlist_blockers": blocker_counts(watchlist),
        "aggregate_by_direction": aggregate_results(validation, ("direction_mode",)),
        "aggregate_by_side": aggregate_results(validation, ("side_policy",)),
        "aggregate_by_direction_side": direction_side,
        "aggregate_by_direction_side_fill": direction_side_fill,
        "aggregate_by_direction_window": aggregate_results(validation, ("direction_mode", "window_policy")),
        "aggregate_by_direction_liquidity": aggregate_results(validation, ("direction_mode", "liquidity_policy")),
        "aggregate_by_regime": dict(list(regime.items())[:30]),
        "paired_by_direction_side": aggregate_paired(paired, ("direction_mode", "side_policy")),
        "top_paired": top_paired(paired),
        "support_equivalence_findings": support_equivalence(validation),
        "gate_rows": len(gate_rows),
        "verdict": scientific_verdict(decision_counts, watchlist),
    }
    return result


def scientific_verdict(decision_counts: Counter[str], watchlist: list[dict[str, str]]) -> dict[str, Any]:
    if decision_counts.get("deployable_candidate", 0):
        decision = "review_deployable_candidates"
    elif watchlist:
        decision = "research_only_no_deployable_edge"
    else:
        decision = "reject_current_matrix"
    return {
        "decision": decision,
        "side_policy_interpretation": (
            "Do not interpret UP-only or DOWN-only as a deployable rule. In this matrix, side "
            "often collapses to support availability: Both can equal UpOnly or DownOnly because "
            "the other side has zero selected validation trades under the same gates."
        ),
        "next_test": (
            "Recover sample power for inverted/distance-contrarian entry-only candidates while "
            "keeping event-held-out validation, then rerun with side-neutral stability gates."
        ),
    }


def markdown(report: dict[str, Any]) -> str:
    lines = [
        "# Dry-Run Correction Matrix Attribution",
        "",
        f"- Run: `{report.get('run_id') or 'unknown'}`",
        f"- Snapshot hash: `{report.get('snapshot_hash')}`",
        f"- Hypotheses: `{report.get('hypothesis_count')}`",
        f"- Min trades: `{report.get('min_trades')}`",
        f"- Decisions: `{report.get('decision_counts')}`",
        f"- Verdict: `{report['verdict']['decision']}`",
        "",
        "## Key Interpretation",
        "",
        f"- {report['verdict']['side_policy_interpretation']}",
        f"- {report['verdict']['next_test']}",
        "",
        "## Direction x Side",
        "",
        "| Direction / Side | Rows | Trades | Net PnL | Avg PnL/Trade | Positive Row Rate |",
        "| --- | ---: | ---: | ---: | ---: | ---: |",
    ]
    for key, data in report["aggregate_by_direction_side"].items():
        avg = data["avg_pnl_per_trade"]
        pos = data["positive_row_rate"]
        lines.append(
            f"| {key} | {data['rows']} | {data['trades']} | {data['net_pnl']:.2f} | "
            f"{avg if avg is not None else 'n/a'} | {pos if pos is not None else 'n/a'} |"
        )

    lines.extend(["", "## Watchlist Blockers", ""])
    for reason, count in report["watchlist_blockers"].items():
        lines.append(f"- `{reason}`: `{count}`")

    lines.extend(["", "## Top Paired Rows", ""])
    for row in report["top_paired"][:10]:
        lines.append(
            f"- `{row['hypothesis']}`: `{row['decision']}`, train `{row['train_net_pnl']:.2f}`/"
            f"`{row['train_trades']}`, validation `{row['val_net_pnl']:.2f}`/`{row['val_trades']}`, "
            f"reason `{row['reason']}`"
        )

    lines.extend(["", "## Side Support Equivalence", ""])
    if report["support_equivalence_findings"]:
        for row in report["support_equivalence_findings"][:10]:
            ctx = ", ".join(f"{key}={value}" for key, value in row["context"].items())
            lines.append(
                f"- `{row['kind']}` trades `{row['trades']}`, PnL `{row['net_pnl']:.2f}` under {ctx}"
            )
    else:
        lines.append("- No side-support equivalence detected.")
    lines.append("")
    return "\n".join(lines)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--artifact-dir", required=True)
    parser.add_argument("--run-id")
    parser.add_argument("--output-json")
    parser.add_argument("--output-md")
    args = parser.parse_args()

    report = build_report(Path(args.artifact_dir), args.run_id)
    if args.output_json:
        Path(args.output_json).write_text(json.dumps(report, indent=2, sort_keys=True) + "\n")
    if args.output_md:
        Path(args.output_md).write_text(markdown(report))
    if not args.output_json and not args.output_md:
        print(json.dumps(report, indent=2, sort_keys=True))


if __name__ == "__main__":
    main()
