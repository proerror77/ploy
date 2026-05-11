#!/usr/bin/env python3
"""Attach official settlement prices to replay runtime evidence events."""

from __future__ import annotations

import argparse
import json
from pathlib import Path
from typing import Any


def load_settlement_prices(path: Path) -> dict[tuple[str, str], str]:
    with path.open(encoding="utf-8") as handle:
        payload = json.load(handle)
    if isinstance(payload, dict):
        payload = payload.get("settlements", [])
    prices: dict[tuple[str, str], str] = {}
    if not isinstance(payload, list):
        return prices
    for item in payload:
        if not isinstance(item, dict):
            continue
        event_id = item.get("event_id")
        token_id = item.get("token_id")
        settlement = item.get("settlement")
        if event_id is None or token_id is None or settlement is None:
            continue
        prices[(str(event_id), str(token_id))] = str(settlement)
    return prices


def is_open_settlement(value: Any) -> bool:
    if value is None:
        return True
    return str(value).strip().lower() in {"", "open"}


def enrich_payload(payload: dict[str, Any], prices: dict[tuple[str, str], str]) -> dict[str, int]:
    events = ((payload.get("runtime_evidence") or {}).get("events") or [])
    stats = {
        "runtime_events": 0,
        "runtime_events_open": 0,
        "runtime_events_settlement_enriched": 0,
        "settlement_price_count": len(prices),
    }
    if not isinstance(events, list):
        return stats
    for event in events:
        if not isinstance(event, dict):
            continue
        stats["runtime_events"] += 1
        if not is_open_settlement(event.get("settlement")):
            continue
        stats["runtime_events_open"] += 1
        event_id = event.get("event_id") or event.get("market_id")
        token_id = event.get("token_id")
        if event_id is None or token_id is None:
            continue
        settlement = prices.get((str(event_id), str(token_id)))
        if settlement is None:
            continue
        event["settlement"] = settlement
        stats["runtime_events_settlement_enriched"] += 1
    runtime_evidence = payload.setdefault("runtime_evidence", {})
    if isinstance(runtime_evidence, dict):
        runtime_evidence["settlement_enrichment"] = stats
    return stats


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--input-json", required=True)
    parser.add_argument("--output-json", required=True)
    parser.add_argument("--settlements-json", required=True)
    parser.add_argument("--report-json", required=True)
    args = parser.parse_args()

    input_path = Path(args.input_json)
    with input_path.open(encoding="utf-8") as handle:
        payload = json.load(handle)
    if not isinstance(payload, dict):
        raise SystemExit(f"{input_path}: expected JSON object")

    stats = enrich_payload(payload, load_settlement_prices(Path(args.settlements_json)))

    output_path = Path(args.output_json)
    output_path.parent.mkdir(parents=True, exist_ok=True)
    with output_path.open("w", encoding="utf-8") as handle:
        json.dump(payload, handle, indent=2, sort_keys=True)
        handle.write("\n")

    report_path = Path(args.report_json)
    report_path.parent.mkdir(parents=True, exist_ok=True)
    with report_path.open("w", encoding="utf-8") as handle:
        json.dump(stats, handle, indent=2, sort_keys=True)
        handle.write("\n")
    print(json.dumps(stats, indent=2, sort_keys=True))


if __name__ == "__main__":
    main()
