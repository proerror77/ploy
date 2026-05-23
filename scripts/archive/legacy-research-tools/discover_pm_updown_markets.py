#!/usr/bin/env python3
"""
Discover current/upcoming Polymarket crypto up/down markets with canonical token IDs.

This script uses the external `polymarket` CLI as the discovery source because
Gamma metadata exposes `clobTokenIds` as hex strings, while CLOB orderbook
queries require decimal token IDs. The output is a manifest that the repo can
use to seed fresh collection runs and validate that token IDs are healthy before
starting PM/Binance/Deribit capture.
"""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
from dataclasses import dataclass
from datetime import UTC, datetime, timedelta
from typing import Any
from zoneinfo import ZoneInfo


ET = ZoneInfo("America/New_York")
ASSET_TO_SYMBOL = {
    "btc": "BTCUSDT",
    "eth": "ETHUSDT",
    "sol": "SOLUSDT",
}
ASSET_TO_NAME = {
    "btc": "Bitcoin",
    "eth": "Ethereum",
    "sol": "Solana",
}


def run_json(*args: str) -> Any:
    raw = subprocess.check_output(args, text=True)
    return json.loads(raw)


def hex_to_decimal_string(hex_token: str) -> str:
    token = hex_token.strip()
    if token.startswith(("0x", "0X")):
        return str(int(token, 16))
    return token


def discover_queries(asset: str, lookahead_hours: int, now_et: datetime) -> list[str]:
    asset_name = ASSET_TO_NAME[asset]
    queries: list[str] = []
    seen: set[str] = set()
    total_hours = max(1, lookahead_hours)

    for hour_offset in range(total_hours + 1):
        dt = now_et + timedelta(hours=hour_offset)
        query = f"{asset_name} Up or Down - {dt.strftime('%B')} {dt.day}, {dt.hour % 12 or 12}"
        if query not in seen:
            seen.add(query)
            queries.append(query)

    day_query = f"{asset_name} Up or Down - {now_et.strftime('%B')} {now_et.day}"
    if day_query not in seen:
        queries.append(day_query)

    end_et = now_et + timedelta(hours=lookahead_hours)
    if end_et.date() != now_et.date():
        next_day_query = f"{asset_name} Up or Down - {end_et.strftime('%B')} {end_et.day}"
        if next_day_query not in seen:
            queries.append(next_day_query)

    return queries


def market_matches(
    market: dict[str, Any],
    asset: str,
    timeframe: str,
    now_utc: datetime,
    lookahead_hours: int,
) -> bool:
    slug = str(market.get("slug") or "")
    question = str(market.get("question") or "")
    event_start_raw = market.get("eventStartTime")
    if not event_start_raw:
        return False

    try:
        event_start = datetime.fromisoformat(str(event_start_raw).replace("Z", "+00:00"))
    except ValueError:
        return False

    slug_prefix = f"{asset}-updown-{timeframe}-"
    if not slug.startswith(slug_prefix):
        return False

    if market.get("closed") is True:
        return False

    if market.get("acceptingOrders") is not True:
        return False

    if event_start < now_utc - timedelta(minutes=1):
        return False

    if event_start > now_utc + timedelta(hours=lookahead_hours):
        return False

    return "Up or Down" in question


def validate_book(decimal_token_id: str) -> dict[str, Any]:
    book = run_json("polymarket", "clob", "book", decimal_token_id, "-o", "json")
    bids = [float(level["price"]) for level in book.get("bids") or []]
    asks = [float(level["price"]) for level in book.get("asks") or []]
    return {
        "best_bid": max(bids) if bids else None,
        "best_ask": min(asks) if asks else None,
        "bid_levels": len(bids),
        "ask_levels": len(asks),
        "book_ok": bool(bids or asks),
    }


def price_to_beat_status(market: dict[str, Any]) -> dict[str, Any]:
    raw = market.get("groupItemThreshold")
    try:
        threshold = float(raw) if raw is not None else None
    except (TypeError, ValueError):
        threshold = None

    if threshold is not None and threshold > 1.0:
        return {
            "status": "metadata_threshold_available",
            "metadata_threshold": threshold,
            "note": "Gamma metadata already exposes a usable fixed threshold.",
        }

    return {
        "status": "capture_at_event_start",
        "metadata_threshold": threshold,
        "note": (
            "Relative up/down markets expose groupItemThreshold=0. "
            "Capture Chainlink BTC/USD at eventStartTime as the canonical price_to_beat."
        ),
    }


def build_record(asset: str, market: dict[str, Any]) -> dict[str, Any]:
    outcomes = json.loads(str(market["outcomes"]))
    token_hex_ids = json.loads(str(market["clobTokenIds"]))

    tokens: list[dict[str, Any]] = []
    for outcome_name, token_hex in zip(outcomes, token_hex_ids, strict=True):
        token_id = hex_to_decimal_string(token_hex)
        book = validate_book(token_id)
        tokens.append(
            {
                "outcome": outcome_name,
                "token_hex": token_hex,
                "token_id": token_id,
                **book,
            }
        )

    return {
        "market_id": market.get("id"),
        "condition_id": market.get("conditionId"),
        "slug": market.get("slug"),
        "question": market.get("question"),
        "symbol": ASSET_TO_SYMBOL[asset],
        "resolution_source": market.get("resolutionSource"),
        "event_start_time": market.get("eventStartTime"),
        "end_time": market.get("endDate"),
        "accepting_orders": market.get("acceptingOrders"),
        "best_bid_market": market.get("bestBid"),
        "best_ask_market": market.get("bestAsk"),
        "last_trade_price": market.get("lastTradePrice"),
        "price_to_beat": price_to_beat_status(market),
        "tokens": tokens,
    }


def render_sql(records: list[dict[str, Any]]) -> str:
    lines = []
    for record in records:
        target_date = record["event_start_time"][:10]
        expires_at = record["end_time"]
        for token in record["tokens"]:
            metadata = {
                "symbol": record["symbol"],
                "slug": record["slug"],
                "question": record["question"],
                "side": token["outcome"],
                "token_hex": token["token_hex"],
                "condition_id": record["condition_id"],
                "event_start_time": record["event_start_time"],
                "end_time": record["end_time"],
                "resolution_source": record["resolution_source"],
                "price_to_beat_status": record["price_to_beat"]["status"],
            }
            metadata_json = json.dumps(metadata, separators=(",", ":")).replace("'", "''")
            lines.append(
                "INSERT INTO collector_token_targets "
                "(token_id, domain, target_date, expires_at, metadata) VALUES "
                f"('{token['token_id']}', 'CRYPTO', DATE '{target_date}', "
                f"TIMESTAMPTZ '{expires_at}', '{metadata_json}'::jsonb) "
                "ON CONFLICT (token_id) DO UPDATE SET "
                "target_date = EXCLUDED.target_date, "
                "expires_at = EXCLUDED.expires_at, "
                "metadata = EXCLUDED.metadata, "
                "updated_at = NOW();"
            )
    return "\n".join(lines)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--asset", choices=sorted(ASSET_TO_SYMBOL), default="btc")
    parser.add_argument("--timeframe", choices=["5m", "15m"], default="5m")
    parser.add_argument("--lookahead-hours", type=int, default=6)
    parser.add_argument("--limit", type=int, default=200)
    parser.add_argument("--format", choices=["json", "sql"], default="json")
    args = parser.parse_args()

    now_utc = datetime.now(UTC)
    now_et = now_utc.astimezone(ET)

    queries = discover_queries(args.asset, args.lookahead_hours, now_et)
    seen: set[str] = set()
    matches: list[dict[str, Any]] = []

    for query in queries:
        for market in run_json("polymarket", "markets", "search", query, "--limit", str(args.limit), "-o", "json"):
            slug = str(market.get("slug") or "")
            if slug in seen:
                continue
            seen.add(slug)
            if market_matches(market, args.asset, args.timeframe, now_utc, args.lookahead_hours):
                matches.append(build_record(args.asset, market))

    matches.sort(key=lambda item: item["event_start_time"])

    if args.format == "sql":
        print(render_sql(matches))
        return 0

    payload = {
        "generated_at_utc": now_utc.isoformat().replace("+00:00", "Z"),
        "asset": args.asset,
        "symbol": ASSET_TO_SYMBOL[args.asset],
        "timeframe": args.timeframe,
        "lookahead_hours": args.lookahead_hours,
        "records": matches,
        "notes": [
            "token_id is the canonical decimal CLOB token id validated via polymarket clob book",
            "token_hex is the raw Gamma/CLI clobTokenIds value from market metadata",
            "relative up/down markets need price_to_beat captured at eventStartTime from Chainlink",
            "official settlement should be persisted later via pm_token_settlements",
        ],
    }
    json.dump(payload, sys.stdout, indent=2)
    sys.stdout.write("\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
