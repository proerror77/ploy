#!/usr/bin/env python3
"""Build the AutoFactor candidate top-bucket diagnostic artifact.

The source report must come from factor_walk_forward_v2. For settlement targets,
that report scores one decision per event against historical full-depth
executable settlement labels. This script converts the selected
runtime-mappable candidate row into an explicit aggregate diagnostic artifact.

This is not the same as replaying the deployed runtime scorer over an ordered
MarketUpdate stream, so it must not by itself unlock a dry-run handoff.
"""

from __future__ import annotations

import argparse
import csv
import hashlib
import json
from pathlib import Path
from typing import Any


DEFAULT_ALLOWED_TARGETS = {
    "full_depth_settlement_executable_pnl",
    "tradeable_full_depth_settlement_pnl",
}

AGGREGATE_BASIS = "factor_walk_forward_top_bucket_aggregate"

FORMULA_RUNTIME_MAPPED_NAMES = {
    "amplitude_weighted_momentum_30s_sigma",
    "mut_amplitude_weighted_momentum_30s_sigma_full_depth_entry_gate",
    "mut_spread_adjusted_external_move_full_depth_entry_gate",
}

PREDICTIVE_FORMULA_BASES = (
    "amplitude_weighted_momentum_30s_sigma",
    "poly_lag_pressure",
    "spread_adjusted_external_move",
)

SETTLEMENT_FORMULA_BASES = (
    "auto_settlement_full_depth_settlement_edge",
    "auto_settlement_conservative_settlement_edge",
    "auto_settlement_model_full_depth_settlement_edge",
    "auto_settlement_model_conservative_settlement_edge",
)


def parse_float(raw: str, default: float = float("nan")) -> float:
    try:
        return float(raw)
    except (TypeError, ValueError):
        return default


def parse_int(raw: str, default: int = 0) -> int:
    try:
        return int(raw)
    except (TypeError, ValueError):
        return default


def canonical_sha256(payload: dict[str, Any]) -> str:
    encoded = json.dumps(payload, sort_keys=True, separators=(",", ":")).encode("utf-8")
    return hashlib.sha256(encoded).hexdigest()


def replay_identity(*, runtime_score: str, strategy_profile: str, evidence: str, source_factor: dict[str, Any] | None) -> dict[str, Any]:
    return {
        "basis": AGGREGATE_BASIS,
        "runtime_score": runtime_score,
        "strategy_profile": strategy_profile,
        "evidence": evidence,
        "source_factor": source_factor or {},
    }


def normalize_formula_name(name: str) -> str:
    while True:
        for prefix in ("llm_", "mut2_", "mut_", "mcts_"):
            if name.startswith(prefix):
                name = name[len(prefix) :]
                break
        else:
            return name


def is_settlement_predictive_formula(name: str) -> bool:
    normalized = normalize_formula_name(name)
    if normalized == "spread_adjusted_external_move":
        return False
    return any(normalized.startswith(base) for base in PREDICTIVE_FORMULA_BASES)


def is_settlement_formula(name: str) -> bool:
    normalized = normalize_formula_name(name)
    for base in SETTLEMENT_FORMULA_BASES:
        if not normalized.startswith(base):
            continue
        return settlement_formula_suffix_supported(normalized[len(base) :])
    return False


def settlement_formula_suffix_supported(suffix: str) -> bool:
    if not suffix:
        return True
    tokens = suffix.removeprefix("_").split("_")
    applied: set[str] = set()
    for token in tokens:
        effect = {
            "strike": "near_strike",
            "capacity": "capacity",
            "quality": "entry_price_quality",
            "adjusted": "spread_adjusted",
            "pressure": "external_pressure",
            "change": "iv_change",
            "gate": "full_depth_entry_gate",
            "squashed": "squashed",
        }.get(token)
        if effect:
            if effect in applied:
                return False
            applied.add(effect)
            continue
        if token not in {
            "x",
            "near",
            "full",
            "depth",
            "entry",
            "price",
            "spread",
            "external",
            "iv",
        }:
            return False
    return True


def parse_autofactor_rows(report_text: str) -> list[dict[str, Any]]:
    rows: list[dict[str, Any]] = []
    in_section = False
    header: list[str] | None = None
    current_target = ""
    for line in report_text.splitlines():
        if line.startswith("# AutoFactor target="):
            current_target = line.split("=", 1)[1].strip()
            in_section = True
            header = None
            continue
        if not in_section:
            continue
        if line.startswith("rank,name,target,"):
            header = next(csv.reader([line]))
            continue
        if not line.strip():
            in_section = False
            header = None
            current_target = ""
            continue
        if header is None or line.startswith("===") or line.startswith("target labels"):
            continue
        values = next(csv.reader([line]))
        if len(values) != len(header):
            continue
        item = dict(zip(header, values))
        item["target"] = item.get("target") or current_target
        rows.append(item)
    return rows


def runtime_mapping(name: str) -> dict[str, str]:
    if name in {
        "spread_adjusted_external_move",
        "repricing_gap_side_10s",
    }:
        return {
            "strategy_profile": "repricing_momentum",
            "runtime_score": (
                "spread_adjusted_external_move_score"
                if name == "spread_adjusted_external_move"
                else name
            ),
        }
    if (
        is_settlement_formula(name)
        or name in FORMULA_RUNTIME_MAPPED_NAMES
        or is_settlement_predictive_formula(name)
    ):
        return {
            "strategy_profile": "settlement_probability",
            "runtime_score": f"autofactor_formula:{name}",
        }
    return {"strategy_profile": "", "runtime_score": ""}


def row_score(row: dict[str, Any]) -> tuple[float, ...]:
    rank = parse_int(row.get("rank", ""), default=10_000)
    return (
        parse_float(row.get("icir", "")),
        parse_float(row.get("positive_window_ratio", "")),
        parse_float(row.get("symbol_positive_ratio", "")),
        parse_float(row.get("spearman_ic", "")),
        -float(rank),
        parse_float(row.get("top_bucket_avg_label", "")),
    )


def select_candidate(
    rows: list[dict[str, Any]],
    *,
    allowed_targets: set[str],
    required_strategy_profile: str,
) -> tuple[dict[str, Any] | None, list[str]]:
    candidates: list[dict[str, Any]] = []
    blockers: list[str] = []
    for row in rows:
        mapping = runtime_mapping(str(row.get("name", "")))
        row_target = str(row.get("target", ""))
        if row_target not in allowed_targets:
            continue
        if row.get("decision") != "candidate" or row.get("reason") != "passed":
            blockers.append(f"not_candidate:{row.get('name')}:{row.get('decision')}:{row.get('reason')}")
            continue
        if mapping.get("strategy_profile") != required_strategy_profile:
            blockers.append(
                "runtime_profile_mismatch:"
                f"{row.get('name')}:{mapping.get('strategy_profile') or '<missing>'}!="
                f"{required_strategy_profile}"
            )
            continue
        if not mapping.get("runtime_score"):
            blockers.append(f"missing_runtime_score:{row.get('name')}")
            continue
        row = dict(row)
        row["runtime_mapping"] = mapping
        candidates.append(row)
    if not candidates:
        return None, blockers or ["no_runtime_mappable_candidate"]
    return max(candidates, key=row_score), []


def build_artifact(
    row: dict[str, Any] | None,
    *,
    blockers: list[str],
    stake_usd: float,
    required_strategy_profile: str,
    evidence: str,
    min_trade_count: int,
    min_fill_rate: float,
    min_roi: float,
) -> dict[str, Any]:
    if row is None:
        identity = replay_identity(
            runtime_score="",
            strategy_profile=required_strategy_profile,
            evidence=evidence,
            source_factor=None,
        )
        return {
            "schema_version": 1,
            "kind": "autofactor_candidate_strategy_replay",
            "candidate_replay_id": f"candidate_replay:{canonical_sha256(identity)[:32]}",
            "identity": identity,
            "evidence_stage": "diagnostic",
            "basis": AGGREGATE_BASIS,
            "strategy_profile": required_strategy_profile,
            "runtime_score": "",
            "promotion_ready": False,
            "promotion_decision": "blocked",
            "evidence": evidence,
            "decision_contract": {
                "event_level": True,
                "one_decision_per_event": True,
                "official_settlement": True,
                "full_depth_entry": True,
                "stake_usd": stake_usd,
            },
            "metrics": {},
            "blocking_risk_flags": blockers,
        }

    top_bucket_n = parse_int(row.get("top_bucket_n", "0"))
    unique_events = parse_int(row.get("top_bucket_unique_event_count", "0"))
    max_event_decisions = parse_int(row.get("top_bucket_max_event_decisions", "0"))
    avg_label = parse_float(row.get("top_bucket_avg_label", "nan"))
    fill_rate = parse_float(row.get("top_bucket_full_depth_entry_fill_rate", "nan"))
    total_pnl = avg_label * top_bucket_n if avg_label == avg_label else float("nan")
    notional = stake_usd * top_bucket_n
    roi = total_pnl / notional if notional > 0 and total_pnl == total_pnl else float("nan")
    runtime_score = row["runtime_mapping"]["runtime_score"]
    source_factor = {
        "name": row.get("name", ""),
        "target": row.get("target", ""),
        "decision": row.get("decision", ""),
        "reason": row.get("reason", ""),
    }
    identity = replay_identity(
        runtime_score=runtime_score,
        strategy_profile=row["runtime_mapping"]["strategy_profile"],
        evidence=evidence,
        source_factor=source_factor,
    )

    artifact_blockers = list(blockers)
    artifact_blockers.append("requires_runtime_replay_not_top_bucket_aggregate")
    if top_bucket_n < min_trade_count:
        artifact_blockers.append(f"trade_count_too_small:{top_bucket_n}<{min_trade_count}")
    if unique_events < min(top_bucket_n, min_trade_count):
        artifact_blockers.append(
            f"unique_event_count_too_small:{unique_events}<{min(top_bucket_n, min_trade_count)}"
        )
    if max_event_decisions != 1:
        artifact_blockers.append(f"one_event_decision_violation:{max_event_decisions}")
    if not (fill_rate == fill_rate) or fill_rate < min_fill_rate:
        artifact_blockers.append(f"entry_fill_rate_too_low:{fill_rate:.4f}<{min_fill_rate:.4f}")
    if not (roi == roi) or roi < min_roi:
        artifact_blockers.append(f"roi_too_low:{roi:.6f}<{min_roi:.6f}")

    return {
        "schema_version": 1,
        "kind": "autofactor_candidate_strategy_replay",
        "candidate_replay_id": f"candidate_replay:{canonical_sha256(identity)[:32]}",
        "identity": identity,
        "evidence_stage": "diagnostic",
        "basis": AGGREGATE_BASIS,
        "strategy_profile": row["runtime_mapping"]["strategy_profile"],
        "runtime_score": runtime_score,
        "promotion_ready": False,
        "promotion_decision": "blocked",
        "evidence": evidence,
        "source_factor": source_factor,
        "decision_contract": {
            "event_level": True,
            "one_decision_per_event": True,
            "official_settlement": True,
            "full_depth_entry": True,
            "stake_usd": stake_usd,
            "entry_policy": "top_scoring_bucket",
        },
        "metrics": {
            "trade_count": top_bucket_n,
            "unique_event_count": unique_events,
            "total_pnl": total_pnl,
            "roi": roi,
            "entry_fill_rate": fill_rate,
            "top_bucket_positive_label_rate": parse_float(
                row.get("top_bucket_positive_label_rate", "nan")
            ),
            "avg_entry_sweep_slip_bps": parse_float(
                row.get("top_bucket_avg_entry_sweep_slip_bps")
                or row.get("top_bucket_avg_entry_sweep_slippage_bps")
                or "nan"
            ),
            "avg_entry_sweep_levels": parse_float(
                row.get("top_bucket_avg_entry_sweep_levels", "nan")
            ),
            "max_event_decisions": max_event_decisions,
        },
        "blocking_risk_flags": artifact_blockers,
    }


def render_markdown(artifact: dict[str, Any]) -> str:
    metrics = artifact.get("metrics") or {}
    lines = [
        "# AutoFactor Candidate Strategy Replay",
        "",
        f"- Promotion ready: `{str(artifact.get('promotion_ready')).lower()}`",
        f"- Runtime score: `{artifact.get('runtime_score') or '<none>'}`",
        f"- Strategy profile: `{artifact.get('strategy_profile') or '<none>'}`",
        f"- Evidence stage: `{artifact.get('evidence_stage')}`",
        f"- Trades: `{metrics.get('trade_count', 'n/a')}`",
        f"- Unique events: `{metrics.get('unique_event_count', 'n/a')}`",
        f"- Total PnL: `{metrics.get('total_pnl', 'n/a')}`",
        f"- ROI: `{metrics.get('roi', 'n/a')}`",
        f"- Entry fill rate: `{metrics.get('entry_fill_rate', 'n/a')}`",
        "",
        "## Blocking Risk Flags",
        "",
    ]
    flags = artifact.get("blocking_risk_flags") or []
    if flags:
        lines.extend(f"- `{flag}`" for flag in flags)
    else:
        lines.append("- `<none>`")
    lines.append("")
    return "\n".join(lines)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--report", required=True)
    parser.add_argument("--output-json", required=True)
    parser.add_argument("--output-md", default="")
    parser.add_argument("--allowed-target", action="append", default=[])
    parser.add_argument("--required-strategy-profile", default="settlement_probability")
    parser.add_argument("--stake-usd", type=float, default=15.0)
    parser.add_argument("--min-trade-count", type=int, default=50)
    parser.add_argument("--min-fill-rate", type=float, default=0.30)
    parser.add_argument("--min-roi", type=float, default=0.0)
    parser.add_argument("--evidence", default="")
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    report_path = Path(args.report)
    rows = parse_autofactor_rows(report_path.read_text(encoding="utf-8"))
    allowed_targets = set(args.allowed_target or DEFAULT_ALLOWED_TARGETS)
    row, blockers = select_candidate(
        rows,
        allowed_targets=allowed_targets,
        required_strategy_profile=args.required_strategy_profile,
    )
    artifact = build_artifact(
        row,
        blockers=blockers,
        stake_usd=args.stake_usd,
        required_strategy_profile=args.required_strategy_profile,
        evidence=args.evidence or str(report_path),
        min_trade_count=args.min_trade_count,
        min_fill_rate=args.min_fill_rate,
        min_roi=args.min_roi,
    )
    output_json = Path(args.output_json)
    output_json.parent.mkdir(parents=True, exist_ok=True)
    output_json.write_text(json.dumps(artifact, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    if args.output_md:
        output_md = Path(args.output_md)
        output_md.parent.mkdir(parents=True, exist_ok=True)
        output_md.write_text(render_markdown(artifact), encoding="utf-8")
    print(json.dumps(artifact, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
