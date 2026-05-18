#!/usr/bin/env python3
"""Build a true runtime-replay candidate strategy artifact.

This consumes the JSON written by `ploy-runner run --output-json` after running
the candidate strategy config over an ordered MarketUpdate replay. Unlike the
AutoFactor top-bucket aggregate helper, this artifact represents the deployed
runtime scorer's actual intent/order/fill behavior on the replay stream.
"""

from __future__ import annotations

import argparse
import json
from collections import Counter
from decimal import Decimal, InvalidOperation
from pathlib import Path
from typing import Any


RUNTIME_REPLAY_BASIS = "runtime_market_update_replay"


def decimal_value(raw: Any, default: Decimal = Decimal("0")) -> Decimal:
    try:
        if raw is None:
            return default
        return Decimal(str(raw))
    except (InvalidOperation, ValueError):
        return default


def is_closed_settlement(raw: Any) -> bool:
    if raw is None:
        return False
    text = str(raw).strip().lower()
    return text not in {"", "open", "none", "null", "nan"}


def runtime_evidence(payload: dict[str, Any]) -> dict[str, Any]:
    evidence = payload.get("runtime_evidence")
    return evidence if isinstance(evidence, dict) else {}


def result_payload(payload: dict[str, Any]) -> dict[str, Any]:
    result = payload.get("result")
    return result if isinstance(result, dict) else {}


def purpose_is_entry(row: dict[str, Any]) -> bool:
    intent_id = str(row.get("intent_id") or "")
    side = str(row.get("side") or row.get("fill_side") or row.get("order_side") or "").upper()
    if intent_id.startswith("tl_settle_") or side == "SELL":
        return False
    purpose = row.get("purpose") or row.get("signal_inputs", {}).get("purpose")
    if purpose is not None:
        return str(purpose).upper() == "ENTRY"
    return side == "BUY"


def row_key(row: dict[str, Any], key: str) -> str | None:
    raw = row.get(key)
    if raw is None:
        return None
    text = str(raw).strip()
    return text or None


def build_event_id_lookup(rows: list[dict[str, Any]]) -> dict[tuple[str, str], str]:
    lookup: dict[tuple[str, str], str] = {}
    for row in rows:
        event_id = row_key(row, "event_id") or row_key(row, "market_id")
        if not event_id:
            continue
        for key in ("intent_id", "order_id", "token_id"):
            value = row_key(row, key)
            if value:
                lookup[(key, value)] = event_id
    return lookup


def resolved_event_id(row: dict[str, Any], lookup: dict[tuple[str, str], str]) -> str | None:
    event_id = row_key(row, "event_id") or row_key(row, "market_id")
    if event_id:
        return event_id
    for key in ("intent_id", "order_id", "token_id"):
        value = row_key(row, key)
        if value and (key, value) in lookup:
            return lookup[(key, value)]
    return None


def build_artifact(
    runtime_eval: dict[str, Any],
    *,
    runtime_score: str,
    strategy_profile: str,
    stake_usd: Decimal,
    full_depth_entry: bool,
    min_trade_count: int,
    min_fill_rate: Decimal,
    min_roi: Decimal,
    evidence: str,
) -> dict[str, Any]:
    evidence_rows = runtime_evidence(runtime_eval)
    result = result_payload(runtime_eval)
    events = [row for row in evidence_rows.get("events") or [] if isinstance(row, dict)]
    orders = [row for row in evidence_rows.get("orders") or [] if isinstance(row, dict)]
    fills = [row for row in evidence_rows.get("fills") or [] if isinstance(row, dict)]
    intents = [row for row in evidence_rows.get("intents") or [] if isinstance(row, dict)]

    entry_orders = [row for row in orders if purpose_is_entry(row)]
    entry_fills = [row for row in fills if purpose_is_entry(row)]
    entry_events = [row for row in events if purpose_is_entry(row)]

    event_lookup = build_event_id_lookup(events + intents + orders + fills)
    decision_rows = entry_events or entry_orders
    event_ids = [
        event_id
        for row in decision_rows
        if (event_id := resolved_event_id(row, event_lookup))
    ]
    unique_event_count = len(set(event_ids))
    event_decision_counts = Counter(event_ids)
    max_event_decisions = max(event_decision_counts.values(), default=0)

    filled_order_ids = {
        str(row.get("order_id"))
        for row in entry_fills
        if row.get("order_id") and decimal_value(row.get("quantity")) > 0
    }
    trade_count = len(filled_order_ids)
    if entry_events:
        filled_entry_event_ids = {
            event_id
            for row in entry_events
            if (
                str(row.get("fill_status") or "").upper() == "FILLED"
                or (row_key(row, "order_id") in filled_order_ids)
            )
            and (event_id := resolved_event_id(row, event_lookup))
        }
        trade_count = len(filled_entry_event_ids)
    entry_order_ids = {order_id for row in entry_orders if (order_id := row_key(row, "order_id"))}
    entry_order_ids.update(
        order_id for row in entry_events if (order_id := row_key(row, "order_id"))
    )
    order_count = len(entry_order_ids)
    entry_intents = [row for row in intents if purpose_is_entry(row)]
    intent_count = max(len(entry_intents), len(entry_events))
    if intent_count == 0:
        intent_count = int(result.get("intents_submitted") or 0)
    denominator = max(order_count, intent_count, 1)
    entry_fill_rate = Decimal(trade_count) / Decimal(denominator)

    settled_event_count = sum(1 for row in entry_events if is_closed_settlement(row.get("settlement")))
    total_pnl = sum((decimal_value(row.get("pnl")) for row in entry_events), Decimal("0"))
    if not entry_events:
        total_pnl = decimal_value(result.get("net_pnl"), decimal_value(result.get("realized_pnl")))
    notional = stake_usd * Decimal(trade_count)
    roi = total_pnl / notional if notional > 0 else Decimal("0")

    blocking_flags: list[str] = []
    if trade_count < min_trade_count:
        blocking_flags.append(f"trade_count_too_small:{trade_count}<{min_trade_count}")
    if unique_event_count < min(trade_count, min_trade_count):
        blocking_flags.append(
            f"unique_event_count_too_small:{unique_event_count}<{min(trade_count, min_trade_count)}"
        )
    if max_event_decisions != 1 and trade_count > 0:
        blocking_flags.append(f"one_event_decision_violation:{max_event_decisions}")
    if entry_fill_rate < min_fill_rate:
        blocking_flags.append(
            f"entry_fill_rate_too_low:{entry_fill_rate:.4f}<{min_fill_rate:.4f}"
        )
    if settled_event_count < trade_count:
        blocking_flags.append(f"official_settlement_missing:{settled_event_count}<{trade_count}")
    if not full_depth_entry:
        blocking_flags.append("full_depth_entry_not_confirmed")
    if roi < min_roi:
        blocking_flags.append(f"roi_too_low:{roi:.6f}<{min_roi:.6f}")
    if not entry_orders and not entry_fills:
        blocking_flags.append("zero_runtime_orders_and_fills")

    diagnostics = runtime_eval.get("strategy_diagnostics") or result.get("strategy_diagnostics") or {}

    return {
        "schema_version": 1,
        "kind": "autofactor_candidate_strategy_replay",
        "evidence_stage": "executable_replay",
        "basis": RUNTIME_REPLAY_BASIS,
        "strategy_profile": strategy_profile,
        "runtime_score": runtime_score,
        "promotion_ready": not blocking_flags,
        "evidence": evidence,
        "decision_contract": {
            "event_level": trade_count > 0 and unique_event_count == trade_count,
            "one_decision_per_event": trade_count > 0 and max_event_decisions == 1,
            "official_settlement": trade_count > 0 and settled_event_count >= trade_count,
            "full_depth_entry": full_depth_entry,
            "stake_usd": float(stake_usd),
        },
        "metrics": {
            "updates_processed": int(result.get("updates_processed") or 0),
            "intents_submitted": intent_count,
            "runtime_intents_submitted": int(result.get("intents_submitted") or intent_count),
            "orders": order_count,
            "fills": len(entry_fills),
            "trade_count": trade_count,
            "unique_event_count": unique_event_count,
            "settlement_event_count": settled_event_count,
            "max_event_decisions": max_event_decisions,
            "entry_fill_rate": float(entry_fill_rate),
            "total_pnl": float(total_pnl),
            "roi": float(roi),
        },
        "strategy_diagnostics": diagnostics,
        "blocking_risk_flags": blocking_flags,
    }


def render_markdown(artifact: dict[str, Any]) -> str:
    metrics = artifact.get("metrics") or {}
    flags = artifact.get("blocking_risk_flags") or []
    lines = [
        "# Runtime Candidate Strategy Replay",
        "",
        f"- Promotion ready: `{str(artifact.get('promotion_ready')).lower()}`",
        f"- Basis: `{artifact.get('basis')}`",
        f"- Runtime score: `{artifact.get('runtime_score')}`",
        f"- Strategy profile: `{artifact.get('strategy_profile')}`",
        f"- Updates processed: `{metrics.get('updates_processed')}`",
        f"- Intents submitted: `{metrics.get('intents_submitted')}`",
        f"- Trades: `{metrics.get('trade_count')}`",
        f"- Unique events: `{metrics.get('unique_event_count')}`",
        f"- Entry fill rate: `{metrics.get('entry_fill_rate')}`",
        f"- Total PnL: `{metrics.get('total_pnl')}`",
        f"- ROI: `{metrics.get('roi')}`",
        "",
        "## Blocking Risk Flags",
        "",
    ]
    lines.extend(f"- `{flag}`" for flag in flags) if flags else lines.append("- `<none>`")
    lines.append("")
    return "\n".join(lines)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--runtime-evaluation-json", required=True)
    parser.add_argument("--runtime-score", required=True)
    parser.add_argument("--strategy-profile", default="settlement_probability")
    parser.add_argument("--stake-usd", default="15")
    parser.add_argument("--full-depth-entry", action="store_true")
    parser.add_argument("--min-trade-count", type=int, default=50)
    parser.add_argument("--min-fill-rate", default="0.30")
    parser.add_argument("--min-roi", default="0")
    parser.add_argument("--evidence", default="")
    parser.add_argument("--output-json", required=True)
    parser.add_argument("--output-md", default="")
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    runtime_path = Path(args.runtime_evaluation_json)
    runtime_eval = json.loads(runtime_path.read_text(encoding="utf-8"))
    artifact = build_artifact(
        runtime_eval,
        runtime_score=args.runtime_score,
        strategy_profile=args.strategy_profile,
        stake_usd=decimal_value(args.stake_usd),
        full_depth_entry=args.full_depth_entry,
        min_trade_count=args.min_trade_count,
        min_fill_rate=decimal_value(args.min_fill_rate),
        min_roi=decimal_value(args.min_roi),
        evidence=args.evidence or str(runtime_path),
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
