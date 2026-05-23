#!/usr/bin/env python3
"""
Reverse-engineered strategy dry run (not copy-trading replay).

This script infers a rule set from one Polymarket profile snapshot, then runs
an independent decision engine on observed market ticks.

Design goal:
- Do not mirror target BUY/SELL side
- Infer strategy parameters from public footprint
- Execute synthetic dry-run trades with risk caps
"""

from __future__ import annotations

import argparse
import dataclasses
import json
import math
import re
import statistics
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
class StrategyParams:
    bias_down_target: float
    entry_window_secs: int
    down_buy_threshold: float
    up_buy_threshold: float
    take_profit: float
    min_trade_usdc: float
    scalp_fraction: float
    hedge_band_low: float
    hedge_band_high: float
    max_event_usdc: float
    max_total_usdc: float


@dataclass(frozen=True)
class DryRunResult:
    executed_buys: int
    executed_sells: int
    skipped_orders: int
    buy_notional: float
    sell_notional: float
    realized_pnl: float
    unrealized_pnl: float
    open_mark_value: float
    down_mark_value: float
    up_mark_value: float
    down_ratio: float

    @property
    def total_pnl(self) -> float:
        return self.realized_pnl + self.unrealized_pnl


def _load_payload(source: str) -> JSON:
    if source.startswith("http://") or source.startswith("https://"):
        with urllib.request.urlopen(source, timeout=20) as resp:
            html = resp.read().decode("utf-8", errors="replace")
        m = re.search(r"<script[^>]*>(\{\"props\":\{.*?\"__N_SSG\":true\}.*?\})</script>", html, re.S)
        if not m:
            raise ValueError("Unable to locate embedded Polymarket JSON payload")
        return json.loads(m.group(1))

    with open(source, "r", encoding="utf-8") as f:
        raw = f.read()
    if raw.lstrip().startswith("<"):
        m = re.search(r"<script[^>]*>(\{\"props\":\{.*?\"__N_SSG\":true\}.*?\})</script>", raw, re.S)
        if not m:
            raise ValueError("Unable to locate embedded Polymarket JSON payload")
        return json.loads(m.group(1))
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


def _clamp(x: float, lo: float, hi: float) -> float:
    return max(lo, min(hi, x))


def _flatten_pages(data: object) -> List[JSON]:
    if not isinstance(data, Mapping):
        return []
    pages = data.get("pages")
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
        raise ValueError("invalid payload: missing props")
    page_props = props.get("pageProps")
    if not isinstance(page_props, Mapping):
        raise ValueError("invalid payload: missing pageProps")

    address = str(
        page_props.get("proxyAddress")
        or page_props.get("primaryAddress")
        or page_props.get("baseAddress")
        or ""
    )

    dehydrated = page_props.get("dehydratedState")
    if not isinstance(dehydrated, Mapping):
        raise ValueError("invalid payload: missing dehydratedState")
    queries = dehydrated.get("queries")
    if not isinstance(queries, list):
        raise ValueError("invalid payload: missing queries")

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


def _parse_event_end_ts(event_slug: str) -> int:
    # updown slugs usually end with epoch start timestamp, e.g. ...-1772332200.
    m = re.search(r"-(\d{9,11})$", event_slug)
    if not m:
        return 0
    start_ts = int(m.group(1))
    duration = 900 if "-15m-" in event_slug else 300 if "-5m-" in event_slug else 3600
    return start_ts + duration


def _percentile(values: Sequence[float], p: float, default: float) -> float:
    clean = sorted(v for v in values if v > 0)
    if not clean:
        return default
    idx = int(round((len(clean) - 1) * _clamp(p, 0.0, 1.0)))
    return clean[idx]


def infer_strategy_params(
    snapshot: ProfileSnapshot,
    *,
    min_trade_usdc: float,
    max_event_usdc: float,
    max_total_usdc: float,
) -> StrategyParams:
    trades = [x for x in snapshot.activity if x.raw_type == "TRADE" and x.price > 0 and x.size > 0]

    buy_down = [x.price for x in trades if x.side == "BUY" and x.outcome.lower() == "down"]
    buy_up = [x.price for x in trades if x.side == "BUY" and x.outcome.lower() == "up"]

    # Infer directional bias from current inventory value.
    down_val = 0.0
    up_val = 0.0
    for row in snapshot.positions:
        v = _to_float(row.get("currentValue"))
        outcome = str(row.get("outcome") or "").lower()
        if outcome == "down":
            down_val += v
        elif outcome == "up":
            up_val += v
    total_val = down_val + up_val
    inferred_bias = (down_val / total_val) if total_val > 0 else 0.65
    bias_down_target = _clamp(inferred_bias, 0.55, 0.85)

    # Estimate trading time window near settlement from event slug timestamps.
    tte_values: List[float] = []
    for x in trades:
        end_ts = _parse_event_end_ts(x.event_slug)
        if end_ts <= 0 or x.timestamp <= 0:
            continue
        tte = end_ts - x.timestamp
        if 0 <= tte <= 600:
            tte_values.append(float(tte))
    if tte_values:
        entry_window_secs = int(_clamp(_percentile(tte_values, 0.80, 120.0), 30.0, 300.0))
    else:
        entry_window_secs = 120

    down_buy_threshold = _clamp(_percentile(buy_down, 0.70, 0.58), 0.10, 0.90)
    up_buy_threshold = _clamp(_percentile(buy_up, 0.60, 0.35), 0.05, 0.80)

    # Infer scalp take-profit from event/outcome buy-vs-sell spread where possible.
    per_leg: Dict[Tuple[str, str], Dict[str, List[float]]] = defaultdict(lambda: {"BUY": [], "SELL": []})
    for x in trades:
        per_leg[(x.event_slug, x.outcome)][x.side].append(x.price)
    diffs: List[float] = []
    for sides in per_leg.values():
        b = sides["BUY"]
        s = sides["SELL"]
        if b and s:
            diffs.append((statistics.mean(s) - statistics.mean(b)))
    positive = [x for x in diffs if x > 0]
    take_profit = _clamp(_percentile(positive, 0.50, 0.02), 0.005, 0.08)

    hedge_band_low = _clamp(bias_down_target - 0.18, 0.45, 0.80)
    hedge_band_high = _clamp(bias_down_target + 0.18, 0.55, 0.92)

    return StrategyParams(
        bias_down_target=bias_down_target,
        entry_window_secs=entry_window_secs,
        down_buy_threshold=down_buy_threshold,
        up_buy_threshold=up_buy_threshold,
        take_profit=take_profit,
        min_trade_usdc=max(min_trade_usdc, 1.0),
        scalp_fraction=0.35,
        hedge_band_low=hedge_band_low,
        hedge_band_high=hedge_band_high,
        max_event_usdc=max(max_event_usdc, 0.0),
        max_total_usdc=max(max_total_usdc, 0.0),
    )


def _title_allowed(title: str, target_assets: Sequence[str]) -> bool:
    if not target_assets:
        return True
    upper = title.upper()
    return any(k.upper() in upper for k in target_assets)


def _mark_prices_from_positions(positions: Iterable[JSON]) -> Dict[Tuple[str, str], float]:
    out: Dict[Tuple[str, str], float] = {}
    for row in positions:
        event_slug = str(row.get("eventSlug") or "")
        outcome = str(row.get("outcome") or "")
        px = _to_float(row.get("curPrice"))
        if event_slug and outcome and px > 0:
            out[(event_slug, outcome)] = px
    return out


def _portfolio_down_ratio(
    inventory: Mapping[Tuple[str, str], List[Tuple[float, float]]],
    latest_px: Mapping[Tuple[str, str], float],
) -> float:
    down = 0.0
    up = 0.0
    for key, lots in inventory.items():
        event_slug, outcome = key
        px = _to_float(latest_px.get((event_slug, outcome), 0.0))
        if px <= 0:
            continue
        qty = sum(q for q, _ in lots)
        val = qty * px
        if outcome.lower() == "down":
            down += val
        elif outcome.lower() == "up":
            up += val
    total = down + up
    return (down / total) if total > 0 else 0.5


def run_reverse_strategy_dry_run(
    *,
    snapshot: ProfileSnapshot,
    params: StrategyParams,
    target_assets: Sequence[str],
) -> DryRunResult:
    ticks = [x for x in snapshot.activity if x.raw_type == "TRADE" and x.price > 0 and x.size > 0]
    ticks.sort(key=lambda x: x.timestamp)

    inventory: MutableMapping[Tuple[str, str], List[Tuple[float, float]]] = defaultdict(list)
    latest_px: MutableMapping[Tuple[str, str], float] = {}
    event_spend: MutableMapping[str, float] = defaultdict(float)

    buy_notional = 0.0
    sell_notional = 0.0
    realized = 0.0
    buys = 0
    sells = 0
    skipped = 0

    def execute_buy(ev: TradeEvent) -> None:
        nonlocal buy_notional, buys, skipped
        usdc = params.min_trade_usdc
        if params.max_event_usdc > 0 and (event_spend[ev.event_slug] + usdc) > params.max_event_usdc:
            skipped += 1
            return
        if params.max_total_usdc > 0 and (buy_notional + usdc) > params.max_total_usdc:
            skipped += 1
            return
        qty = usdc / ev.price
        inventory[(ev.event_slug, ev.outcome)].append((qty, ev.price))
        event_spend[ev.event_slug] += usdc
        buy_notional += usdc
        buys += 1

    def execute_sell(ev: TradeEvent, fraction: float) -> None:
        nonlocal sell_notional, sells, realized, skipped
        key = (ev.event_slug, ev.outcome)
        lots = inventory.get(key)
        if not lots:
            skipped += 1
            return
        total_qty = sum(q for q, _ in lots)
        if total_qty <= 0:
            skipped += 1
            return
        target_qty = total_qty * _clamp(fraction, 0.0, 1.0)
        remain = target_qty
        closed = 0.0
        while remain > 1e-9 and lots:
            q, entry = lots[0]
            take = min(remain, q)
            realized += (ev.price - entry) * take
            q -= take
            remain -= take
            closed += take
            if q <= 1e-9:
                lots.pop(0)
            else:
                lots[0] = (q, entry)
        if closed <= 0:
            skipped += 1
            return
        sell_notional += closed * ev.price
        sells += 1

    for ev in ticks:
        if not _title_allowed(ev.title, target_assets):
            continue

        key = (ev.event_slug, ev.outcome)
        latest_px[key] = ev.price
        end_ts = _parse_event_end_ts(ev.event_slug)
        if end_ts <= 0:
            continue
        tte = end_ts - ev.timestamp
        if tte < 0:
            continue

        down_ratio = _portfolio_down_ratio(inventory, latest_px)
        if tte <= params.entry_window_secs:
            if ev.outcome.lower() == "down":
                if ev.price <= params.down_buy_threshold and down_ratio <= params.hedge_band_high:
                    execute_buy(ev)
            elif ev.outcome.lower() == "up":
                if ev.price <= params.up_buy_threshold and down_ratio >= params.hedge_band_low:
                    execute_buy(ev)

        lots = inventory.get(key, [])
        if lots:
            avg_entry = sum(q * p for q, p in lots) / max(sum(q for q, _ in lots), 1e-9)
            if ev.price >= avg_entry + params.take_profit:
                execute_sell(ev, params.scalp_fraction)
            elif tte <= 20 and ev.price >= avg_entry + max(params.take_profit * 0.6, 0.01):
                execute_sell(ev, 0.20)

    mark_px = _mark_prices_from_positions(snapshot.positions)
    for k, v in latest_px.items():
        mark_px.setdefault(k, v)

    unrealized = 0.0
    mark_value = 0.0
    down_value = 0.0
    up_value = 0.0
    for (event_slug, outcome), lots in inventory.items():
        px = _to_float(mark_px.get((event_slug, outcome), 0.0))
        if px <= 0:
            continue
        qty = sum(q for q, _ in lots)
        val = qty * px
        cost = sum(q * p for q, p in lots)
        mark_value += val
        unrealized += (val - cost)
        if outcome.lower() == "down":
            down_value += val
        elif outcome.lower() == "up":
            up_value += val

    down_ratio = (down_value / (down_value + up_value)) if (down_value + up_value) > 0 else 0.5
    return DryRunResult(
        executed_buys=buys,
        executed_sells=sells,
        skipped_orders=skipped,
        buy_notional=buy_notional,
        sell_notional=sell_notional,
        realized_pnl=realized,
        unrealized_pnl=unrealized,
        open_mark_value=mark_value,
        down_mark_value=down_value,
        up_mark_value=up_value,
        down_ratio=down_ratio,
    )


def main(argv: Sequence[str]) -> int:
    parser = argparse.ArgumentParser(description="Reverse-engineered strategy dry run")
    parser.add_argument(
        "--source",
        default="https://polymarket.com/zh/@k9Q2mX4L8A7ZP3R",
        help="Profile URL or local payload file",
    )
    parser.add_argument("--min-trade-usdc", type=float, default=5.0)
    parser.add_argument("--max-event-usdc", type=float, default=250.0)
    parser.add_argument("--max-total-usdc", type=float, default=2000.0)
    parser.add_argument("--target-assets", default="Bitcoin,Ethereum,Solana,XRP")
    parser.add_argument("--json-out", default="")
    args = parser.parse_args(argv)

    payload = _load_payload(args.source)
    snapshot = extract_profile_snapshot(payload)
    params = infer_strategy_params(
        snapshot,
        min_trade_usdc=args.min_trade_usdc,
        max_event_usdc=args.max_event_usdc,
        max_total_usdc=args.max_total_usdc,
    )
    assets = tuple(x.strip() for x in args.target_assets.split(",") if x.strip())
    result = run_reverse_strategy_dry_run(snapshot=snapshot, params=params, target_assets=assets)

    out = {
        "address": snapshot.address,
        "source": args.source,
        "inferred_params": dataclasses.asdict(params),
        "dry_run_result": dataclasses.asdict(result) | {"total_pnl": result.total_pnl},
    }

    print(json.dumps(out, ensure_ascii=False, indent=2))
    if args.json_out:
        with open(args.json_out, "w", encoding="utf-8") as f:
            json.dump(out, f, ensure_ascii=False, indent=2)
            f.write("\n")
        print(f"saved: {args.json_out}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
