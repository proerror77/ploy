#!/usr/bin/env python3
"""Evaluate whether AutoFactor report rows can become strategy handoffs.

This is intentionally stricter than the AutoFactor IC/ICIR candidate gate.
An AutoFactor row is only a qualified strategy when:

1. all global settlement PRD promotion gates are ready;
2. the row is a `candidate` with reason `passed`;
3. the target is one of the allowed executable targets; and
4. the factor has an explicit runtime strategy-profile mapping.

For `autofactor_formula:*` runtime scores, the PRD model-specific
`symbol_holdout` and `walk_forward_oos` gates are replaced by formula-level
symbol/window stability from the AutoFactor row. Data quality, Deribit,
execution-depth, calibration, and replay-parity gates remain global blockers.

The current PM5D/PM15D settlement PRD should not silently promote a good
repricing factor into the settlement strategy lane.
"""

from __future__ import annotations

import argparse
import csv
import json
from dataclasses import asdict, dataclass, field
from pathlib import Path
from typing import Any


DEFAULT_ALLOWED_TARGETS = ("full_depth_settlement_executable_pnl",)

SETTLEMENT_RUNTIME_MAPPING = {
    "strategy_profile": "settlement_probability",
    "strategy_family": "settlement_probability",
}

BUILTIN_RUNTIME_MAPPINGS: dict[str, dict[str, str]] = {
    "spread_adjusted_external_move": {
        "strategy_profile": "repricing_momentum",
        "strategy_family": "repricing",
        "runtime_score": "spread_adjusted_external_move_score",
    },
    "repricing_gap_side_10s": {
        "strategy_profile": "repricing_momentum",
        "strategy_family": "repricing",
        "runtime_score": "repricing_gap_side_10s",
    },
    "settlement_fair_edge": {
        "strategy_profile": "",
        "strategy_family": "settlement_probability",
        "runtime_score": "",
    },
    "auto_settlement_full_depth_settlement_edge": {
        **SETTLEMENT_RUNTIME_MAPPING,
        "runtime_score": "autofactor_formula:auto_settlement_full_depth_settlement_edge",
    },
    "auto_settlement_full_depth_settlement_edge_x_near_strike": {
        **SETTLEMENT_RUNTIME_MAPPING,
        "runtime_score": "autofactor_formula:auto_settlement_full_depth_settlement_edge_x_near_strike",
    },
    "auto_settlement_full_depth_settlement_edge_x_capacity": {
        **SETTLEMENT_RUNTIME_MAPPING,
        "runtime_score": "autofactor_formula:auto_settlement_full_depth_settlement_edge_x_capacity",
    },
    "auto_settlement_full_depth_settlement_edge_x_near_strike_x_capacity": {
        **SETTLEMENT_RUNTIME_MAPPING,
        "runtime_score": "autofactor_formula:auto_settlement_full_depth_settlement_edge_x_near_strike_x_capacity",
    },
    "auto_settlement_full_depth_settlement_edge_spread_adjusted": {
        **SETTLEMENT_RUNTIME_MAPPING,
        "runtime_score": "autofactor_formula:auto_settlement_full_depth_settlement_edge_spread_adjusted",
    },
    "auto_settlement_full_depth_settlement_edge_x_external_pressure": {
        **SETTLEMENT_RUNTIME_MAPPING,
        "runtime_score": "autofactor_formula:auto_settlement_full_depth_settlement_edge_x_external_pressure",
    },
    "auto_settlement_full_depth_settlement_edge_x_iv_change": {
        **SETTLEMENT_RUNTIME_MAPPING,
        "runtime_score": "autofactor_formula:auto_settlement_full_depth_settlement_edge_x_iv_change",
    },
    "auto_settlement_conservative_settlement_edge": {
        **SETTLEMENT_RUNTIME_MAPPING,
        "runtime_score": "autofactor_formula:auto_settlement_conservative_settlement_edge",
    },
    "auto_settlement_conservative_settlement_edge_x_near_strike": {
        **SETTLEMENT_RUNTIME_MAPPING,
        "runtime_score": "autofactor_formula:auto_settlement_conservative_settlement_edge_x_near_strike",
    },
    "auto_settlement_conservative_settlement_edge_x_capacity": {
        **SETTLEMENT_RUNTIME_MAPPING,
        "runtime_score": "autofactor_formula:auto_settlement_conservative_settlement_edge_x_capacity",
    },
    "auto_settlement_conservative_settlement_edge_x_near_strike_x_capacity": {
        **SETTLEMENT_RUNTIME_MAPPING,
        "runtime_score": "autofactor_formula:auto_settlement_conservative_settlement_edge_x_near_strike_x_capacity",
    },
    "auto_settlement_conservative_settlement_edge_spread_adjusted": {
        **SETTLEMENT_RUNTIME_MAPPING,
        "runtime_score": "autofactor_formula:auto_settlement_conservative_settlement_edge_spread_adjusted",
    },
    "auto_settlement_conservative_settlement_edge_x_external_pressure": {
        **SETTLEMENT_RUNTIME_MAPPING,
        "runtime_score": "autofactor_formula:auto_settlement_conservative_settlement_edge_x_external_pressure",
    },
    "auto_settlement_conservative_settlement_edge_x_iv_change": {
        **SETTLEMENT_RUNTIME_MAPPING,
        "runtime_score": "autofactor_formula:auto_settlement_conservative_settlement_edge_x_iv_change",
    },
}


@dataclass
class PromotionGate:
    ready: bool
    evidence: str
    blocked_gates: list[str] = field(default_factory=list)


@dataclass
class AutoFactorRow:
    rank: int
    name: str
    target: str
    decision: str
    reason: str
    n: int
    spearman_ic: float
    pearson_ic: float
    window_count: int
    icir: float
    positive_window_ratio: float
    symbol_count: int
    symbol_positive_ratio: float
    monotonicity: float
    top_bucket_avg_label: float
    top_bucket_positive_label_rate: float
    complexity: int


@dataclass
class EvaluatedFactor:
    factor: AutoFactorRow
    qualified: bool
    blockers: list[str]
    runtime_mapping: dict[str, str] | None


def factor_metrics(row: AutoFactorRow) -> dict[str, Any]:
    return {
        "rank": row.rank,
        "n": row.n,
        "spearman_ic": row.spearman_ic,
        "pearson_ic": row.pearson_ic,
        "window_count": row.window_count,
        "icir": row.icir,
        "positive_window_ratio": row.positive_window_ratio,
        "symbol_count": row.symbol_count,
        "symbol_positive_ratio": row.symbol_positive_ratio,
        "monotonicity": row.monotonicity,
        "top_bucket_avg_label": row.top_bucket_avg_label,
        "top_bucket_positive_label_rate": row.top_bucket_positive_label_rate,
        "complexity": row.complexity,
    }


def parse_float(raw: str) -> float:
    try:
        return float(raw)
    except ValueError:
        return float("nan")


def parse_int(raw: str) -> int:
    try:
        return int(raw)
    except ValueError:
        return 0


def parse_promotion_gate(report_text: str) -> PromotionGate:
    ready: bool | None = None
    evidence = ""
    blocked: list[str] = []
    in_gate_table = False

    for line in report_text.splitlines():
        if line.startswith("ready_for_dry_run_handoff="):
            evidence = line
            ready = line.split("=", 1)[1].split(maxsplit=1)[0].lower() == "true"
            continue
        if line == "gate,passed,evidence":
            in_gate_table = True
            continue
        if in_gate_table:
            if not line.strip():
                break
            parts = line.split(",", 2)
            if len(parts) != 3:
                continue
            gate, passed, gate_evidence = parts
            if passed.lower() != "true":
                blocked.append(f"{gate}: {gate_evidence}")

    if ready is None:
        return PromotionGate(
            ready=False,
            evidence="missing ready_for_dry_run_handoff line",
            blocked_gates=["missing_promotion_gate"],
        )
    return PromotionGate(ready=ready, evidence=evidence, blocked_gates=blocked)


def parse_autofactor_rows(report_text: str) -> list[AutoFactorRow]:
    rows: list[AutoFactorRow] = []
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
        rows.append(
            AutoFactorRow(
                rank=parse_int(item["rank"]),
                name=item["name"],
                target=item.get("target") or current_target,
                decision=item["decision"],
                reason=item["reason"],
                n=parse_int(item["n"]),
                spearman_ic=parse_float(item["spearman_ic"]),
                pearson_ic=parse_float(item["pearson_ic"]),
                window_count=parse_int(item["window_count"]),
                icir=parse_float(item["icir"]),
                positive_window_ratio=parse_float(item["positive_window_ratio"]),
                symbol_count=parse_int(item.get("symbol_count", "0")),
                symbol_positive_ratio=parse_float(item.get("symbol_positive_ratio", "nan")),
                monotonicity=parse_float(item["monotonicity"]),
                top_bucket_avg_label=parse_float(item["top_bucket_avg_label"]),
                top_bucket_positive_label_rate=parse_float(item["top_bucket_positive_label_rate"]),
                complexity=parse_int(item["complexity"]),
            )
        )

    return rows


def load_runtime_mappings(path: str | None) -> dict[str, dict[str, str]]:
    mappings = dict(BUILTIN_RUNTIME_MAPPINGS)
    if not path:
        return mappings
    payload = json.loads(Path(path).read_text(encoding="utf-8"))
    for name, value in payload.items():
        if isinstance(value, dict):
            mappings[name] = {str(k): str(v) for k, v in value.items()}
    return mappings


MODEL_SPECIFIC_PRD_GATE_PREFIXES = (
    "symbol_holdout:",
    "walk_forward_oos:",
)


def is_autofactor_formula(mapping: dict[str, str] | None) -> bool:
    if not mapping:
        return False
    return mapping.get("runtime_score", "").startswith("autofactor_formula:")


def global_gate_blockers(gate: PromotionGate, *, formula_specific: bool) -> list[str]:
    if gate.ready:
        return []
    if not formula_specific:
        return ["promotion_gate_not_ready"]
    blockers = [
        item
        for item in gate.blocked_gates
        if not item.startswith(MODEL_SPECIFIC_PRD_GATE_PREFIXES)
    ]
    if blockers:
        return [f"global_promotion_gate_not_ready:{item}" for item in blockers]
    return []


def evaluate(
    report_text: str,
    *,
    allowed_targets: tuple[str, ...],
    required_strategy_profile: str,
    runtime_mappings: dict[str, dict[str, str]],
) -> dict[str, Any]:
    gate = parse_promotion_gate(report_text)
    rows = parse_autofactor_rows(report_text)
    evaluated: list[EvaluatedFactor] = []

    for row in rows:
        blockers: list[str] = []
        mapping = runtime_mappings.get(row.name)
        formula_specific = is_autofactor_formula(mapping)
        blockers.extend(global_gate_blockers(gate, formula_specific=formula_specific))
        if row.target not in allowed_targets:
            blockers.append("target_not_allowed")
        if row.decision != "candidate" or row.reason != "passed":
            blockers.append(f"autofactor_not_candidate:{row.decision}:{row.reason}")
        if not mapping:
            blockers.append("missing_runtime_strategy_mapping")
        else:
            mapped_profile = mapping.get("strategy_profile", "")
            if not mapped_profile:
                blockers.append("empty_runtime_strategy_profile")
            elif mapped_profile != required_strategy_profile:
                blockers.append(
                    f"runtime_profile_mismatch:{mapped_profile}!={required_strategy_profile}"
                )
        if formula_specific:
            if row.symbol_count < 2:
                blockers.append("formula_symbol_holdout_too_few_symbols")
            elif row.symbol_positive_ratio < 0.60:
                blockers.append(
                    f"formula_symbol_holdout_unstable:{row.symbol_positive_ratio:.4f}<0.60"
                )
        evaluated.append(
            EvaluatedFactor(
                factor=row,
                qualified=not blockers,
                blockers=blockers,
                runtime_mapping=mapping,
            )
        )

    qualified = [item for item in evaluated if item.qualified]
    return {
        "schema_version": 1,
        "decision": "qualified" if qualified else "blocked",
        "required_strategy_profile": required_strategy_profile,
        "allowed_targets": list(allowed_targets),
        "promotion_gate": asdict(gate),
        "qualified_strategies": [
            {
                "factor": asdict(item.factor),
                "runtime_mapping": item.runtime_mapping,
            }
            for item in qualified
        ],
        "evaluated_factors": [
            {
                "factor": asdict(item.factor),
                "qualified": item.qualified,
                "blockers": item.blockers,
                "runtime_mapping": item.runtime_mapping,
            }
            for item in evaluated
        ],
    }


def build_factor_registry(result: dict[str, Any]) -> dict[str, Any]:
    entries = []
    for item in result["evaluated_factors"]:
        factor = item["factor"]
        mapping = item["runtime_mapping"] or {}
        entries.append(
            {
                "name": factor["name"],
                "target": factor["target"],
                "status": "qualified" if item["qualified"] else "blocked",
                "autofactor_decision": factor["decision"],
                "autofactor_reason": factor["reason"],
                "blockers": item["blockers"],
                "runtime_mapping": mapping,
                "metrics": factor_metrics(AutoFactorRow(**factor)),
            }
        )
    return {
        "schema_version": 1,
        "kind": "autofactor_strategy_registry",
        "decision": result["decision"],
        "required_strategy_profile": result["required_strategy_profile"],
        "allowed_targets": result["allowed_targets"],
        "promotion_gate": result["promotion_gate"],
        "entries": entries,
    }


def build_strategy_handoff(result: dict[str, Any]) -> dict[str, Any]:
    strategies = []
    for item in result["qualified_strategies"]:
        factor = item["factor"]
        mapping = item["runtime_mapping"] or {}
        strategies.append(
            {
                "name": factor["name"],
                "target": factor["target"],
                "strategy_profile": mapping.get("strategy_profile", ""),
                "strategy_family": mapping.get("strategy_family", ""),
                "runtime_score": mapping.get("runtime_score", ""),
                "promotion_status": "ready_for_dry_run_handoff",
                "metrics": factor_metrics(AutoFactorRow(**factor)),
            }
        )

    return {
        "schema_version": 1,
        "kind": "autofactor_strategy_handoff",
        "status": "ready" if strategies else "blocked",
        "recommended_action": "create_dry_run_handoff" if strategies else "do_not_promote",
        "required_strategy_profile": result["required_strategy_profile"],
        "allowed_targets": result["allowed_targets"],
        "promotion_gate": result["promotion_gate"],
        "strategies": strategies,
        "blocked_factor_count": sum(
            1 for item in result["evaluated_factors"] if not item["qualified"]
        ),
    }


def render_handoff_markdown(handoff: dict[str, Any]) -> str:
    lines = [
        "# AutoFactor Dry-Run Strategy Handoff",
        "",
        f"Status: `{handoff['status']}`",
        f"Recommended action: `{handoff['recommended_action']}`",
        f"Required strategy profile: `{handoff['required_strategy_profile']}`",
        f"Allowed targets: `{', '.join(handoff['allowed_targets'])}`",
        "",
    ]
    if handoff["status"] != "ready":
        lines.extend(
            [
                "No dry-run handoff issue or config should be created from this artifact.",
                "",
                f"Blocked factor count: `{handoff['blocked_factor_count']}`",
                "",
            ]
        )
        return "\n".join(lines)

    lines.extend(
        [
            "## Draft Issue",
            "",
            "Title: Promote AutoFactor strategy handoff to dry-run",
            "",
            "## Qualified Strategies",
            "",
            "| factor | target | strategy profile | runtime score | icir | top bucket avg label |",
            "| --- | --- | --- | --- | --- | --- |",
        ]
    )
    for strategy in handoff["strategies"]:
        metrics = strategy["metrics"]
        lines.append(
            "| {name} | {target} | {profile} | {runtime_score} | {icir:.6f} | {label:.6f} |".format(
                name=strategy["name"],
                target=strategy["target"],
                profile=strategy["strategy_profile"],
                runtime_score=strategy["runtime_score"],
                icir=metrics["icir"],
                label=metrics["top_bucket_avg_label"],
            )
        )

    lines.extend(
        [
            "",
            "## Dry-Run Config Contract",
            "",
            "```toml",
        ]
    )
    for idx, strategy in enumerate(handoff["strategies"], start=1):
        lines.extend(
            [
                f"[[autofactor_strategy_handoff]] # {idx}",
                f'name = "{strategy["name"]}"',
                f'target = "{strategy["target"]}"',
                f'strategy_profile = "{strategy["strategy_profile"]}"',
                f'strategy_family = "{strategy["strategy_family"]}"',
                f'runtime_score = "{strategy["runtime_score"]}"',
                'promotion_status = "ready_for_dry_run_handoff"',
                "",
            ]
        )
    lines.extend(
        [
            "```",
            "",
            "## Acceptance Criteria",
            "",
            "- Confirm the runtime score is implemented in the shared scorer.",
            "- Confirm the dry-run config uses the same full-depth execution gates as research.",
            "- Confirm replay parity remains ready for the source report.",
            "- Start with small fixed stake and the existing kill switch.",
            "",
        ]
    )
    return "\n".join(lines)


def render_markdown(result: dict[str, Any]) -> str:
    lines = [
        "# AutoFactor Strategy Promotion Report",
        "",
        f"Decision: `{result['decision']}`",
        f"Required strategy profile: `{result['required_strategy_profile']}`",
        f"Allowed targets: `{', '.join(result['allowed_targets'])}`",
        "",
        "## Promotion Gate",
        "",
        f"- Ready: `{str(result['promotion_gate']['ready']).lower()}`",
        f"- Evidence: `{result['promotion_gate']['evidence']}`",
        "",
        "## Qualified Strategies",
        "",
    ]
    if result["qualified_strategies"]:
        lines.append("| factor | target | runtime profile | icir | top bucket avg label |")
        lines.append("| --- | --- | --- | --- | --- |")
        for item in result["qualified_strategies"]:
            factor = item["factor"]
            mapping = item["runtime_mapping"] or {}
            lines.append(
                "| {name} | {target} | {profile} | {icir:.6f} | {label:.6f} |".format(
                    name=factor["name"],
                    target=factor["target"],
                    profile=mapping.get("strategy_profile", ""),
                    icir=factor["icir"],
                    label=factor["top_bucket_avg_label"],
                )
            )
    else:
        lines.append("No AutoFactor row qualifies for strategy handoff under the requested profile.")

    lines.extend(["", "## Evaluated Factors", ""])
    lines.append("| factor | target | decision | blockers |")
    lines.append("| --- | --- | --- | --- |")
    for item in result["evaluated_factors"]:
        factor = item["factor"]
        blockers = ", ".join(item["blockers"]) if item["blockers"] else "none"
        lines.append(
            f"| {factor['name']} | {factor['target']} | {factor['decision']} | {blockers} |"
        )
    lines.append("")
    return "\n".join(lines)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--report", required=True, help="factor_walk_forward_v2 report.txt")
    parser.add_argument("--output-json", default="")
    parser.add_argument("--output-md", default="")
    parser.add_argument("--output-registry-json", default="")
    parser.add_argument("--output-handoff-json", default="")
    parser.add_argument("--output-handoff-md", default="")
    parser.add_argument("--runtime-mapping-json", default="")
    parser.add_argument(
        "--allowed-target",
        action="append",
        default=[],
        help="Allowed AutoFactor target. May be repeated.",
    )
    parser.add_argument("--required-strategy-profile", default="settlement_probability")
    parser.add_argument("--fail-if-blocked", action="store_true")
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    report_text = Path(args.report).read_text(encoding="utf-8")
    allowed_targets = tuple(args.allowed_target or DEFAULT_ALLOWED_TARGETS)
    result = evaluate(
        report_text,
        allowed_targets=allowed_targets,
        required_strategy_profile=args.required_strategy_profile,
        runtime_mappings=load_runtime_mappings(args.runtime_mapping_json or None),
    )

    if args.output_json:
        output = Path(args.output_json)
        output.parent.mkdir(parents=True, exist_ok=True)
        output.write_text(json.dumps(result, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    if args.output_md:
        output = Path(args.output_md)
        output.parent.mkdir(parents=True, exist_ok=True)
        output.write_text(render_markdown(result), encoding="utf-8")
    handoff = build_strategy_handoff(result)
    if args.output_registry_json:
        output = Path(args.output_registry_json)
        output.parent.mkdir(parents=True, exist_ok=True)
        output.write_text(
            json.dumps(build_factor_registry(result), indent=2, sort_keys=True) + "\n",
            encoding="utf-8",
        )
    if args.output_handoff_json:
        output = Path(args.output_handoff_json)
        output.parent.mkdir(parents=True, exist_ok=True)
        output.write_text(
            json.dumps(handoff, indent=2, sort_keys=True) + "\n",
            encoding="utf-8",
        )
    if args.output_handoff_md:
        output = Path(args.output_handoff_md)
        output.parent.mkdir(parents=True, exist_ok=True)
        output.write_text(render_handoff_markdown(handoff), encoding="utf-8")

    print(json.dumps(result, indent=2, sort_keys=True))
    return 0 if result["decision"] == "qualified" or not args.fail_if_blocked else 3


if __name__ == "__main__":
    raise SystemExit(main())
