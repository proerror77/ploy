#!/usr/bin/env python3
"""
Copycat strategy dry-run simulator for Polymarket profile snapshots.

This script replays one wallet's recent activity as a "copy-trading" dry run:
- scales every eligible trade by `--scale`
- enforces basic risk caps
- never sends real orders

Input can be either:
1) a profile page URL (default), or
2) a local JSON payload exported from Polymarket __NEXT_DATA__.
"""

from __future__ import annotations

import argparse
import dataclasses
import datetime as dt
import json
import math
import re
import sys
import urllib.request
from collections import defaultdict
from dataclasses import dataclass
from typing import Dict, Iterable, List, Mapping, MutableMapping, Sequence, Tuple


JSON = Mapping[str, object]


@dataclass(frozen=True)
class TradeEvent:
    event_slug: str
    outcome: str
    side: str
    price: float
    size: float
    usdc_size: float
    timestamp: int
    title: str
    raw_type: str


@dataclass(frozen=True)
class ProfileSnapshot:
    address: str
    activity: List[TradeEvent]
    positions: List[JSON]


@dataclass(frozen=True)
class DryRunResult:
    executed_trades: int
    skipped_trades: int
    executed_buy_usdc: float
    executed_sell_usdc: float
    realized_pnl: float
    unrealized_pnl: float
    open_positions: int
    open_notional: float
    open_mark_value: float
    down_mark_value: float
    up_mark_value: float

    @property
    def total_pnl(self) -> float:
        return self.realized_pnl + self.unrealized_pnl


def _decode_next_payload_from_html(html: str) -> JSON:
    # The profile page ships one large JSON object in a script tag.
    m = re.search(r"<script[^>]*>(\{\"props\":\{.*?\"__N_SSG\":true\}.*?\})</script>", html, re.S)
    if not m:
        raise ValueError("Unable to locate Polymarket embedded JSON payload in HTML.")
    return json.loads(m.group(1))


def _load_payload(source: str) -> JSON:
    if source.startswith("http://") or source.startswith("https://"):
        with urllib.request.urlopen(source, timeout=20) as resp:
            body = resp.read().decode("utf-8", errors="replace")
        return _decode_next_payload_from_html(body)

    with open(source, "r", encoding="utf-8") as f:
        raw = f.read()

    if raw.lstrip().startswith("<"):
        return _decode_next_payload_from_html(raw)
    return json.loads(raw)


def _to_float(value: object) -> float:
    if value is None:
        return 0.0
    try:
        out = float(value)
    except (TypeError, ValueError):
        return 0.0
    if math.isnan(out) or math.isinf(out):
        return 0.0
    return out


def _to_int(value: object) -> int:
    try:
        return int(value)
    except (TypeError, ValueError):
        return 0


def _flatten_pages(state_data: object) -> List[JSON]:
    if not isinstance(state_data, Mapping):
        return []
    pages = state_data.get("pages")
    if not isinstance(pages, list):
        return []
    out: List[JSON] = []
    for page in pages:
        if not isinstance(page, list):
            continue
        for row in page:
            if isinstance(row, Mapping):
                out.append(row)
    return out


def extract_profile_snapshot(payload: JSON) -> ProfileSnapshot:
    props = payload.get("props")
    if not isinstance(props, Mapping):
        raise ValueError("Invalid payload: missing props")
    page_props = props.get("pageProps")
    if not isinstance(page_props, Mapping):
        raise ValueError("Invalid payload: missing pageProps")

    address = str(
        page_props.get("proxyAddress")
        or page_props.get("primaryAddress")
        or page_props.get("baseAddress")
        or ""
    )

    dehydrated = page_props.get("dehydratedState")
    if not isinstance(dehydrated, Mapping):
        raise ValueError("Invalid payload: missing dehydratedState")

    queries = dehydrated.get("queries")
    if not isinstance(queries, list):
        raise ValueError("Invalid payload: missing queries")

    activity_rows: List[JSON] = []
    position_rows: List[JSON] = []

    for query in queries:
        if not isinstance(query, Mapping):
            continue
        key = query.get("queryKey")
        state = query.get("state")
        if not isinstance(key, list) or not isinstance(state, Mapping):
            continue
        data = state.get("data")

        if len(key) >= 2 and key[0] == "profile" and key[1] == "activity":
            activity_rows.extend(_flatten_pages(data))
        if len(key) >= 2 and key[0] == "profile" and key[1] == "positions":
            position_rows.extend(_flatten_pages(data))

    activity: List[TradeEvent] = []
    for row in activity_rows:
        activity.append(
            TradeEvent(
                event_slug=str(row.get("eventSlug") or ""),
                outcome=str(row.get("outcome") or ""),
                side=str(row.get("side") or ""),
                price=_to_float(row.get("price")),
                size=_to_float(row.get("size")),
                usdc_size=_to_float(row.get("usdcSize")),
                timestamp=_to_int(row.get("timestamp")),
                title=str(row.get("title") or ""),
                raw_type=str(row.get("type") or ""),
            )
        )

    activity.sort(key=lambda x: x.timestamp)
    return ProfileSnapshot(address=address, activity=activity, positions=position_rows)


def _asset_allowed(title: str, target_assets: Sequence[str]) -> bool:
    if not target_assets:
        return True
    upper = title.upper()
    return any(asset.upper() in upper for asset in target_assets)


def _build_mark_prices(positions: Iterable[JSON]) -> Dict[Tuple[str, str], float]:
    out: Dict[Tuple[str, str], float] = {}
    for row in positions:
        event_slug = str(row.get("eventSlug") or "")
        outcome = str(row.get("outcome") or "")
        cur_price = _to_float(row.get("curPrice"))
        if not event_slug or not outcome:
            continue
        if cur_price <= 0:
            continue
        out[(event_slug, outcome)] = cur_price
    return out


def run_dry_run(
    *,
    activity: Sequence[TradeEvent],
    mark_prices: Mapping[Tuple[str, str], float],
    scale: float,
    max_event_usdc: float,
    max_total_usdc: float,
    target_assets: Sequence[str],
) -> DryRunResult:
    inventory: MutableMapping[Tuple[str, str], List[Tuple[float, float]]] = defaultdict(list)
    event_spend: MutableMapping[str, float] = defaultdict(float)

    spent = 0.0
    received = 0.0
    realized = 0.0
    executed = 0
    skipped = 0

    for ev in activity:
        if ev.raw_type != "TRADE":
            continue
        if ev.price <= 0 or ev.size <= 0:
            skipped += 1
            continue
        if not _asset_allowed(ev.title, target_assets):
            skipped += 1
            continue

        qty = ev.size * scale
        usdc = ev.usdc_size * scale if ev.usdc_size > 0 else qty * ev.price

        key = (ev.event_slug, ev.outcome)
        if ev.side == "BUY":
            if max_event_usdc > 0 and (event_spend[ev.event_slug] + usdc) > max_event_usdc:
                skipped += 1
                continue
            if max_total_usdc > 0 and (spent + usdc) > max_total_usdc:
                skipped += 1
                continue
            inventory[key].append((qty, ev.price))
            event_spend[ev.event_slug] += usdc
            spent += usdc
            executed += 1
            continue

        if ev.side == "SELL":
            lots = inventory.get(key)
            if not lots:
                skipped += 1
                continue
            remain = qty
            closed_qty = 0.0
            while remain > 1e-9 and lots:
                lot_qty, lot_px = lots[0]
                take = min(remain, lot_qty)
                realized += (ev.price - lot_px) * take
                lot_qty -= take
                remain -= take
                closed_qty += take
                if lot_qty <= 1e-9:
                    lots.pop(0)
                else:
                    lots[0] = (lot_qty, lot_px)
            if closed_qty <= 1e-9:
                skipped += 1
                continue
            received += closed_qty * ev.price
            executed += 1
            continue

        skipped += 1

    unrealized = 0.0
    open_cost = 0.0
    open_mark = 0.0
    down_mark = 0.0
    up_mark = 0.0
    open_positions = 0

    for key, lots in inventory.items():
        if not lots:
            continue
        event_slug, outcome = key
        mark = _to_float(mark_prices.get((event_slug, outcome), 0.0))
        for qty, entry in lots:
            open_positions += 1
            open_cost += qty * entry
            px = mark if mark > 0 else entry
            open_mark += qty * px
            unrealized += (px - entry) * qty
            if outcome.lower() == "down":
                down_mark += qty * px
            if outcome.lower() == "up":
                up_mark += qty * px

    return DryRunResult(
        executed_trades=executed,
        skipped_trades=skipped,
        executed_buy_usdc=spent,
        executed_sell_usdc=received,
        realized_pnl=realized,
        unrealized_pnl=unrealized,
        open_positions=open_positions,
        open_notional=open_cost,
        open_mark_value=open_mark,
        down_mark_value=down_mark,
        up_mark_value=up_mark,
    )


def _format_ts(ts: int) -> str:
    if ts <= 0:
        return "-"
    return dt.datetime.fromtimestamp(ts, dt.timezone.utc).strftime("%Y-%m-%d %H:%M:%S UTC")


def main(argv: Sequence[str]) -> int:
    parser = argparse.ArgumentParser(description="Copycat strategy dry-run simulator")
    parser.add_argument(
        "--source",
        default="https://polymarket.com/zh/@k9Q2mX4L8A7ZP3R",
        help="Profile page URL or local payload file",
    )
    parser.add_argument("--scale", type=float, default=0.10, help="Copy size scaling factor")
    parser.add_argument(
        "--max-event-usdc",
        type=float,
        default=250.0,
        help="Max notional per event in simulation",
    )
    parser.add_argument(
        "--max-total-usdc",
        type=float,
        default=2000.0,
        help="Max total buy notional in simulation",
    )
    parser.add_argument(
        "--target-assets",
        default="Bitcoin,Ethereum,Solana,XRP",
        help="CSV asset filter by title keyword",
    )
    parser.add_argument(
        "--json-out",
        default="",
        help="Optional output json path",
    )
    args = parser.parse_args(argv)

    payload = _load_payload(args.source)
    snapshot = extract_profile_snapshot(payload)
    mark_prices = _build_mark_prices(snapshot.positions)
    assets = tuple(s.strip() for s in args.target_assets.split(",") if s.strip())

    result = run_dry_run(
        activity=snapshot.activity,
        mark_prices=mark_prices,
        scale=max(args.scale, 0.0),
        max_event_usdc=max(args.max_event_usdc, 0.0),
        max_total_usdc=max(args.max_total_usdc, 0.0),
        target_assets=assets,
    )

    trade_rows = [x for x in snapshot.activity if x.raw_type == "TRADE"]
    first_ts = min((x.timestamp for x in trade_rows), default=0)
    last_ts = max((x.timestamp for x in trade_rows), default=0)

    out = {
        "address": snapshot.address,
        "source": args.source,
        "window": {"first_trade": _format_ts(first_ts), "last_trade": _format_ts(last_ts)},
        "scale": args.scale,
        "risk_caps": {
            "max_event_usdc": args.max_event_usdc,
            "max_total_usdc": args.max_total_usdc,
        },
        "summary": dataclasses.asdict(result) | {"total_pnl": result.total_pnl},
    }

    print("=== Copycat Dry Run Summary ===")
    print(json.dumps(out, ensure_ascii=False, indent=2))

    if args.json_out:
        with open(args.json_out, "w", encoding="utf-8") as f:
            json.dump(out, f, ensure_ascii=False, indent=2)
            f.write("\n")
        print(f"saved: {args.json_out}")

    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
