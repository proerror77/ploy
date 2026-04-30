#!/usr/bin/env python3
"""Compare replay/backtest evidence with dry-run report evidence.

The script is intentionally schema-tolerant: current research artifacts and
operator reports are still evolving, so it extracts the common strategy
evaluation fields first and records missing strict parity fields as risk flags.
"""

from __future__ import annotations

import argparse
import json
from datetime import datetime, timezone
from pathlib import Path
from typing import Any


STRICT_FIELDS = [
    "event_id",
    "decision_ts",
    "quote",
    "signal_inputs",
    "side",
    "entry_price",
    "fill_status",
    "settlement",
    "pnl",
]

STRICT_FIELD_ALIASES = {
    "event_id": ("event_id", "market_id", "market_slug", "token_id"),
    "decision_ts": ("decision_ts", "opened_at", "timestamp"),
    "quote": ("quote", "observed_quote"),
    "signal_inputs": ("signal_inputs", "decision_features", "features"),
    "side": ("side", "market_side", "direction"),
    "entry_price": ("entry_price", "avg_entry_price"),
    "fill_status": ("fill_status", "status"),
    "settlement": ("settlement", "resolved_up_won"),
    "pnl": ("pnl", "net_pnl", "realized_pnl"),
}


def load_json(path: Path) -> Any:
    with path.open() as handle:
        return json.load(handle)


def find_first_json(root: Path) -> Path:
    if root.is_file():
        return root
    candidates = sorted(root.rglob("*.json"))
    if not candidates:
        raise SystemExit(f"no JSON files found under {root}")
    preferred = [path for path in candidates if path.name == "evaluation.json"]
    return preferred[0] if preferred else candidates[0]


def get_path(data: Any, *path: str) -> Any:
    current = data
    for key in path:
        if not isinstance(current, dict) or key not in current:
            return None
        current = current[key]
    return current


def extract_metrics(data: Any) -> dict[str, Any]:
    if not isinstance(data, dict):
        return {}
    metrics = data.get("metrics")
    if isinstance(metrics, dict):
        return metrics
    validation = data.get("validation")
    if isinstance(validation, dict):
        return validation
    report = data.get("report")
    if isinstance(report, dict):
        return {
            "health": report.get("health"),
            "review_count": len(report.get("reviews") or []),
        }
    return {}


def extract_events(data: Any) -> list[dict[str, Any]]:
    if isinstance(data, list):
        return [item for item in data if isinstance(item, dict)]
    if not isinstance(data, dict):
        return []
    events: list[dict[str, Any]] = []
    for path in [
        ("events",),
        ("trades",),
        ("fills",),
        ("closed_trades",),
        ("recent_closed",),
        ("open_positions",),
        ("report", "events"),
        ("report", "trades"),
        ("report", "closed_trades"),
        ("report", "recent_closed"),
        ("dry_run", "events"),
        ("data", "events"),
    ]:
        value = get_path(data, *path)
        if isinstance(value, list):
            events.extend(item for item in value if isinstance(item, dict))

    strategies = data.get("strategies")
    if isinstance(strategies, list):
        for strategy in strategies:
            if not isinstance(strategy, dict):
                continue
            for key in ("events", "trades", "fills", "closed_trades", "recent_closed", "open_positions"):
                value = strategy.get(key)
                if isinstance(value, list):
                    for item in value:
                        if isinstance(item, dict):
                            enriched = dict(item)
                            for identity_key in ("runtime_mode", "strategy_id", "deployment_id", "experiment_label"):
                                enriched.setdefault(identity_key, strategy.get(identity_key))
                            events.append(enriched)

    if events:
        deduped = {}
        for idx, event in enumerate(events):
            key = (
                event.get("trade_key")
                or event.get("intent_id")
                or event.get("event_id")
                or event.get("token_id")
                or f"row-{idx}"
            )
            deduped[str(key)] = event
        return list(deduped.values())
    return []


def canonical_field(event: dict[str, Any], field: str) -> Any:
    for key in STRICT_FIELD_ALIASES.get(field, (field,)):
        if key in event and event[key] is not None:
            return event[key]
    return None


def available_strict_fields(events: list[dict[str, Any]]) -> list[str]:
    available = set()
    for event in events:
        for field in STRICT_FIELDS:
            if canonical_field(event, field) is not None:
                available.add(field)
    return sorted(available)


def index_events(events: list[dict[str, Any]]) -> dict[str, dict[str, Any]]:
    indexed = {}
    for idx, event in enumerate(events):
        key = (
            event.get("trade_key")
            or event.get("intent_id")
            or event.get("event_id")
            or event.get("market_id")
            or event.get("market_slug")
            or event.get("token_id")
            or f"row-{idx}"
        )
        indexed[str(key)] = event
    return indexed


def compare_events(
    replay_events: list[dict[str, Any]], dryrun_events: list[dict[str, Any]]
) -> dict[str, Any]:
    replay_index = index_events(replay_events)
    dryrun_index = index_events(dryrun_events)
    shared_keys = sorted(set(replay_index) & set(dryrun_index))
    mismatches: list[dict[str, Any]] = []
    missing_fields = set()
    replay_available = available_strict_fields(replay_events)
    dryrun_available = available_strict_fields(dryrun_events)

    for key in shared_keys:
        replay = replay_index[key]
        dryrun = dryrun_index[key]
        for field in STRICT_FIELDS:
            replay_value = canonical_field(replay, field)
            dryrun_value = canonical_field(dryrun, field)
            if replay_value is None or dryrun_value is None:
                missing_fields.add(field)
                continue
            if replay_value != dryrun_value:
                mismatches.append(
                    {
                        "event_key": key,
                        "field": field,
                        "replay": replay_value,
                        "dryrun": dryrun_value,
                    }
                )

    return {
        "replay_event_count": len(replay_events),
        "dryrun_event_count": len(dryrun_events),
        "shared_event_count": len(shared_keys),
        "strict_required_fields": STRICT_FIELDS,
        "replay_available_strict_fields": replay_available,
        "dryrun_available_strict_fields": dryrun_available,
        "missing_replay_events": sorted(set(dryrun_index) - set(replay_index))[:50],
        "missing_dryrun_events": sorted(set(replay_index) - set(dryrun_index))[:50],
        "mismatches": mismatches[:100],
        "missing_strict_fields": sorted(missing_fields),
        "strict_parity_ready": bool(shared_keys) and not missing_fields and not mismatches,
    }


def build_result(replay: Any, dryrun: Any, replay_path: Path, dryrun_path: Path) -> dict[str, Any]:
    replay_events = extract_events(replay)
    dryrun_events = extract_events(dryrun)
    event_comparison = compare_events(replay_events, dryrun_events)
    risk_flags: list[str] = []

    if not replay_events:
        risk_flags.append("replay_has_no_event_level_rows")
    if not dryrun_events:
        risk_flags.append("dryrun_has_no_event_level_rows")
    if event_comparison["missing_dryrun_events"]:
        risk_flags.append("events_present_in_replay_missing_from_dryrun")
    if event_comparison["missing_replay_events"]:
        risk_flags.append("events_present_in_dryrun_missing_from_replay")
    if event_comparison["mismatches"]:
        risk_flags.append("strict_field_mismatches")
    if event_comparison["missing_strict_fields"]:
        risk_flags.append("missing_strict_parity_fields")

    decision = "continue"
    if not event_comparison["strict_parity_ready"]:
        decision = "fix-data-or-runtime-mismatch"

    return {
        "schema_version": 1,
        "artifact_type": "strategy_parity_evaluation",
        "producer": "replay_dryrun_parity",
        "generated_at": datetime.now(timezone.utc).isoformat(),
        "replay_source": str(replay_path),
        "dryrun_source": str(dryrun_path),
        "replay_metrics": extract_metrics(replay),
        "dryrun_metrics": extract_metrics(dryrun),
        "event_comparison": event_comparison,
        "risk_flags": risk_flags,
        "decision": decision,
    }


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--replay-json", required=True)
    parser.add_argument("--dryrun-json", required=True)
    parser.add_argument("--output-json", required=True)
    args = parser.parse_args()

    replay_path = find_first_json(Path(args.replay_json))
    dryrun_path = find_first_json(Path(args.dryrun_json))
    result = build_result(
        load_json(replay_path),
        load_json(dryrun_path),
        replay_path,
        dryrun_path,
    )

    output_path = Path(args.output_json)
    output_path.parent.mkdir(parents=True, exist_ok=True)
    with output_path.open("w") as handle:
        json.dump(result, handle, indent=2, sort_keys=True)
        handle.write("\n")
    print(json.dumps(result, indent=2, sort_keys=True))


if __name__ == "__main__":
    main()
