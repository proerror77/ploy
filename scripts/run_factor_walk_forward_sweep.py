#!/usr/bin/env python3
"""Run multiple factor_walk_forward_v2 variants after one artifact download/build."""

from __future__ import annotations

import argparse
import json
import re
import shutil
import subprocess
import sys
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Any


SWEEP_KEYS = {
    "label",
    "train_window_days",
    "train_window_hours",
    "test_window_days",
    "test_window_hours",
    "step_days",
    "step_hours",
    "lob_sample_secs",
    "observation_sample_secs",
    "max_quote_age_secs",
    "top_n",
    "min_observations",
    "top_quantile",
    "factor_name_filter",
    "report_suite",
    "data_quality_mode",
    "min_event_complete_events",
    "min_event_complete_rows",
}

OPTIONAL_SWEEP_KEYS = {
    "train_window_hours",
    "test_window_hours",
    "step_hours",
}


@dataclass(frozen=True)
class Variant:
    label: str
    values: dict[str, str]


def slugify(raw: str) -> str:
    slug = re.sub(r"[^A-Za-z0-9_.-]+", "-", raw.strip()).strip("-")
    return slug[:80] or "variant"


def load_sweep(raw: str, base: dict[str, str]) -> list[Variant]:
    if not raw.strip():
        return [Variant(label="base", values=dict(base))]
    payload = json.loads(raw)
    if isinstance(payload, dict):
        variants = payload.get("variants")
    else:
        variants = payload
    if not isinstance(variants, list) or not variants:
        raise SystemExit("sweep_json must be a non-empty JSON array or an object with variants[]")

    parsed: list[Variant] = []
    for index, item in enumerate(variants, start=1):
        if not isinstance(item, dict):
            raise SystemExit(f"sweep variant {index} must be an object")
        unknown = sorted(set(item) - SWEEP_KEYS)
        if unknown:
            raise SystemExit(f"sweep variant {index} has unknown keys: {', '.join(unknown)}")
        values = dict(base)
        for key, value in item.items():
            if key == "label":
                continue
            if isinstance(value, (dict, list)):
                raise SystemExit(f"sweep variant {index} value for {key} must be scalar")
            values[key] = str(value)
        label = str(item.get("label") or values.get("factor_name_filter") or f"variant-{index}")
        parsed.append(Variant(label=slugify(label), values=values))
    return parsed


def factor_args(args: argparse.Namespace, variant: Variant, replay_parity_json: str) -> list[str]:
    values = variant.values
    command = [
        args.binary,
        "--snapshot-dir",
        args.snapshot_dir,
        "--symbols",
        args.symbols,
        "--start-ts",
        args.start_ts,
        "--end-ts",
        args.end_ts,
        "--stake-usd",
        args.stake_usd,
        "--train-window-days",
        values["train_window_days"],
        "--test-window-days",
        values["test_window_days"],
        "--step-days",
        values["step_days"],
        "--lob-sample-secs",
        values["lob_sample_secs"],
        "--observation-sample-secs",
        values["observation_sample_secs"],
        "--max-quote-age-secs",
        values["max_quote_age_secs"],
        "--min-observations",
        values["min_observations"],
        "--top-quantile",
        values["top_quantile"],
        "--top-n",
        values["top_n"],
        "--report-suite",
        values["report_suite"],
        "--data-quality-mode",
        values["data_quality_mode"],
        "--min-event-complete-events",
        values["min_event_complete_events"],
        "--min-event-complete-rows",
        values["min_event_complete_rows"],
    ]
    if values.get("train_window_hours"):
        command.extend(["--train-window-hours", values["train_window_hours"]])
    if values.get("test_window_hours"):
        command.extend(["--test-window-hours", values["test_window_hours"]])
    if values.get("step_hours"):
        command.extend(["--step-hours", values["step_hours"]])
    if values.get("factor_name_filter"):
        command.extend(["--factor-name-filter", values["factor_name_filter"]])
    if replay_parity_json:
        command.extend(["--replay-parity-json", replay_parity_json])
    if args.require_deribit:
        command.append("--require-deribit")
    if args.alpha_search_output_dir:
        command.extend(["--alpha-search-output-dir", args.alpha_search_output_dir])
    if args.alpha_search_plan_json:
        command.extend(["--alpha-search-plan-json", args.alpha_search_plan_json])
    if args.alpha_search_state_json:
        command.extend(["--alpha-search-state-json", args.alpha_search_state_json])
    if args.alpha_search_llm_prior_json:
        command.extend(["--alpha-search-llm-prior-json", args.alpha_search_llm_prior_json])
    return command


def promotion_args(args: argparse.Namespace, report: Path, variant_dir: Path) -> list[str]:
    command = [
        sys.executable,
        args.evaluator,
        "--report",
        str(report),
        "--output-json",
        str(variant_dir / "autofactor-strategy-promotion.json"),
        "--output-md",
        str(variant_dir / "autofactor-strategy-promotion.md"),
        "--output-registry-json",
        str(variant_dir / "autofactor-factor-registry.json"),
        "--output-handoff-json",
        str(variant_dir / "autofactor-strategy-handoff.json"),
        "--output-handoff-md",
        str(variant_dir / "autofactor-strategy-handoff.md"),
        "--required-strategy-profile",
        args.required_strategy_profile,
    ]
    for target in args.allowed_target:
        command.extend(["--allowed-target", target])
    return command


def ranked_factor_rows(promotion: dict[str, Any], allowed_targets: set[str]) -> list[dict[str, Any]]:
    evaluated = promotion.get("evaluated_factors") or []
    rows: list[dict[str, Any]] = []
    for item in evaluated:
        factor = item.get("factor") or {}
        if factor.get("target") not in allowed_targets:
            continue
        mapping = item.get("runtime_mapping") or {}
        blockers = item.get("blockers") or []
        rows.append(
            {
                "name": factor.get("name", ""),
                "target": factor.get("target", ""),
                "decision": factor.get("decision", ""),
                "reason": factor.get("reason", ""),
                "rank": factor.get("rank", 0),
                "spearman_ic": factor.get("spearman_ic"),
                "pearson_ic": factor.get("pearson_ic"),
                "icir": factor.get("icir"),
                "positive_window_ratio": factor.get("positive_window_ratio"),
                "symbol_positive_ratio": factor.get("symbol_positive_ratio"),
                "monotonicity": factor.get("monotonicity"),
                "n": factor.get("n"),
                "window_count": factor.get("window_count"),
                "top_bucket_n": factor.get("top_bucket_n"),
                "top_bucket_avg_label": factor.get("top_bucket_avg_label"),
                "top_bucket_positive_label_rate": factor.get("top_bucket_positive_label_rate"),
                "complexity": factor.get("complexity"),
                "qualified": bool(item.get("qualified")),
                "runtime_mapping": mapping,
                "runtime_mappable": bool(mapping)
                and not any(str(blocker).startswith("runtime_profile_mismatch:") for blocker in blockers)
                and "empty_runtime_strategy_profile" not in blockers,
                "blockers": blockers,
            }
        )
    return rows


def _factor_score(item: dict[str, Any]) -> tuple[float, float, float, float]:
    return (
        1.0 if item["decision"] == "candidate" and item["reason"] == "passed" else 0.0,
        float(item.get("positive_window_ratio") or 0.0),
        float(item.get("symbol_positive_ratio") or 0.0),
        float(item.get("spearman_ic") or 0.0),
    )


def best_factor(promotion: dict[str, Any], allowed_targets: set[str]) -> dict[str, Any] | None:
    candidates = ranked_factor_rows(promotion, allowed_targets)
    if not candidates:
        return None

    def score(item: dict[str, Any]) -> tuple[float, float, float, float, float]:
        return (
            1.0 if item["qualified"] else 0.0,
            *_factor_score(item),
        )

    return max(candidates, key=score)


def best_factor_by_kind(
    promotion: dict[str, Any],
    allowed_targets: set[str],
    *,
    kind: str,
) -> dict[str, Any] | None:
    candidates = ranked_factor_rows(promotion, allowed_targets)
    if kind == "qualified":
        candidates = [item for item in candidates if item["qualified"]]
    elif kind == "runtime_mappable":
        candidates = [item for item in candidates if item["runtime_mappable"]]
    elif kind != "discovery":
        raise ValueError(f"unknown factor kind: {kind}")
    if not candidates:
        return None
    return max(candidates, key=_factor_score)


def write_markdown(summary: dict[str, Any], path: Path) -> None:
    lines = [
        "# Factor Walk-Forward Sweep",
        "",
        f"- Variants: `{len(summary['variants'])}`",
        f"- Total elapsed seconds: `{summary['total_elapsed_seconds']:.2f}`",
        f"- Best variant: `{summary.get('best_variant') or '<none>'}`",
        "",
        "| variant | status | decision | qualified | elapsed_s | best qualified strategy | best runtime-mappable factor | best discovery factor | discovery blockers |",
        "| --- | --- | --- | ---: | ---: | --- | --- | --- | --- |",
    ]
    for item in summary["variants"]:
        discovery = item.get("best_discovery_factor") or item.get("best_factor") or {}
        runtime = item.get("best_runtime_mappable_factor") or {}
        qualified = item.get("best_qualified_strategy") or {}
        lines.append(
            "| {variant} | {status} | {decision} | {qualified_count} | {elapsed:.2f} | {qualified_factor} | {runtime_factor} | {discovery_factor} | {blockers} |".format(
                variant=item["label"],
                status=item["status"],
                decision=item.get("decision") or "",
                qualified_count=item.get("qualified_count", 0),
                elapsed=item.get("elapsed_seconds", 0.0),
                qualified_factor=qualified.get("name", ""),
                runtime_factor=runtime.get("name", ""),
                discovery_factor=discovery.get("name", ""),
                blockers=", ".join(discovery.get("blockers") or []),
            )
        )
    path.write_text("\n".join(lines) + "\n", encoding="utf-8")


def promote_best_variant(best: dict[str, Any] | None, output_dir: Path) -> None:
    if not best:
        return
    source = Path(best["path"])
    for name in [
        "report.txt",
        "autofactor-strategy-promotion.json",
        "autofactor-strategy-promotion.md",
        "autofactor-factor-registry.json",
        "autofactor-strategy-handoff.json",
        "autofactor-strategy-handoff.md",
        "evaluator-output.json",
    ]:
        src = source / name
        if src.exists():
            shutil.copy2(src, output_dir / name)
    (output_dir / "source.txt").write_text(
        f"canonical_result=snapshot_artifact_sweep\nbest_variant={best['label']}\n",
        encoding="utf-8",
    )


def run_variant(
    args: argparse.Namespace,
    variant: Variant,
    index: int,
    replay_parity_json: str,
    allowed_targets: set[str],
) -> dict[str, Any]:
    variant_dir = Path(args.output_dir) / f"{index:03d}-{variant.label}"
    variant_dir.mkdir(parents=True, exist_ok=True)
    (variant_dir / "variant.json").write_text(
        json.dumps({"label": variant.label, "values": variant.values}, indent=2, sort_keys=True),
        encoding="utf-8",
    )
    report_path = variant_dir / "report.txt"
    command = factor_args(args, variant, replay_parity_json)
    started = time.monotonic()
    with report_path.open("w", encoding="utf-8") as report:
        result = subprocess.run(
            command,
            cwd=args.cwd,
            stdout=report,
            stderr=subprocess.STDOUT,
            text=True,
            check=False,
        )
    elapsed = time.monotonic() - started
    item: dict[str, Any] = {
        "label": variant.label,
        "path": str(variant_dir),
        "status": "failed" if result.returncode else "completed",
        "exit_code": result.returncode,
        "elapsed_seconds": elapsed,
        "variant": variant.values,
    }
    if result.returncode:
        return item

    eval_result = subprocess.run(
        promotion_args(args, report_path, variant_dir),
        cwd=args.cwd,
        text=True,
        capture_output=True,
        check=False,
    )
    (variant_dir / "evaluator-output.json").write_text(eval_result.stdout, encoding="utf-8")
    if eval_result.stderr:
        (variant_dir / "evaluator-stderr.txt").write_text(eval_result.stderr, encoding="utf-8")
    item["evaluator_exit_code"] = eval_result.returncode
    if eval_result.returncode not in {0, 3}:
        item["status"] = "evaluation_failed"
        return item
    promotion = json.loads((variant_dir / "autofactor-strategy-promotion.json").read_text(encoding="utf-8"))
    item["decision"] = promotion.get("decision")
    item["qualified_count"] = len(promotion.get("qualified_strategies") or [])
    item["promotion_gate_ready"] = bool((promotion.get("promotion_gate") or {}).get("ready"))
    item["best_factor"] = best_factor(promotion, allowed_targets)
    item["best_discovery_factor"] = best_factor_by_kind(
        promotion,
        allowed_targets,
        kind="discovery",
    )
    item["best_runtime_mappable_factor"] = best_factor_by_kind(
        promotion,
        allowed_targets,
        kind="runtime_mappable",
    )
    item["best_qualified_strategy"] = best_factor_by_kind(
        promotion,
        allowed_targets,
        kind="qualified",
    )
    return item


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", required=True)
    parser.add_argument("--evaluator", default="scripts/evaluate_autofactor_strategy_promotion.py")
    parser.add_argument("--snapshot-dir", required=True)
    parser.add_argument("--output-dir", required=True)
    parser.add_argument("--symbols", required=True)
    parser.add_argument("--start-ts", required=True)
    parser.add_argument("--end-ts", required=True)
    parser.add_argument("--stake-usd", required=True)
    parser.add_argument("--replay-parity-json", default="")
    parser.add_argument("--alpha-search-output-dir", default="")
    parser.add_argument("--alpha-search-plan-json", default="")
    parser.add_argument("--alpha-search-state-json", default="")
    parser.add_argument("--alpha-search-llm-prior-json", default="")
    parser.add_argument("--required-strategy-profile", default="settlement_probability")
    parser.add_argument("--require-deribit", action="store_true")
    parser.add_argument("--allowed-target", action="append", default=[])
    parser.add_argument("--sweep-json", default="")
    parser.add_argument("--cwd", default=".")
    parser.add_argument("--fail-if-all-failed", action="store_true")
    parser.add_argument("--fail-if-blocked", action="store_true")
    parser.add_argument("--dry-run", action="store_true")
    for key in sorted(SWEEP_KEYS - {"label"}):
        parser.add_argument(
            f"--{key.replace('_', '-')}",
            required=key not in OPTIONAL_SWEEP_KEYS,
            default="",
        )
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    base = {key: getattr(args, key) for key in sorted(SWEEP_KEYS - {"label"})}
    variants = load_sweep(args.sweep_json, base)
    output_dir = Path(args.output_dir)
    output_dir.mkdir(parents=True, exist_ok=True)
    allowed_targets = set(args.allowed_target or ["full_depth_settlement_executable_pnl"])

    if args.dry_run:
        summary = {
            "schema_version": "factor_walk_forward_sweep_v1",
            "dry_run": True,
            "variants": [{"label": variant.label, "variant": variant.values} for variant in variants],
        }
        (output_dir / "sweep-summary.json").write_text(
            json.dumps(summary, indent=2, sort_keys=True), encoding="utf-8"
        )
        return 0

    started = time.monotonic()
    variant_summaries = [
        run_variant(args, variant, index, args.replay_parity_json, allowed_targets)
        for index, variant in enumerate(variants, start=1)
    ]
    completed = [item for item in variant_summaries if item["status"] == "completed"]
    best = None
    if completed:
        best = max(
            completed,
            key=lambda item: (
                item.get("qualified_count", 0),
                1.0 if item.get("best_runtime_mappable_factor") else 0.0,
                float(
                    (item.get("best_runtime_mappable_factor") or item.get("best_factor") or {}).get(
                        "positive_window_ratio"
                    )
                    or 0.0
                ),
                float(
                    (item.get("best_runtime_mappable_factor") or item.get("best_factor") or {}).get(
                        "spearman_ic"
                    )
                    or 0.0
                ),
            ),
        )
    summary = {
        "schema_version": "factor_walk_forward_sweep_v1",
        "total_elapsed_seconds": time.monotonic() - started,
        "variant_count": len(variants),
        "completed_count": len(completed),
        "best_variant": best["label"] if best else None,
        "variants": variant_summaries,
    }
    (output_dir / "sweep-summary.json").write_text(
        json.dumps(summary, indent=2, sort_keys=True), encoding="utf-8"
    )
    write_markdown(summary, output_dir / "sweep-summary.md")
    promote_best_variant(best, output_dir)
    if args.fail_if_all_failed and not completed:
        return 2
    if args.fail_if_blocked and (not best or best.get("decision") != "qualified"):
        return 3
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
