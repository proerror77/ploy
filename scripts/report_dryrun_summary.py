#!/usr/bin/env python3
"""Emit dry-run strategy performance summary JSON from the local Ploy database."""

import json
import math
import os
import subprocess
import sys
from collections import defaultdict
from datetime import datetime, timezone
from statistics import mean, stdev


DB_URL = (
    os.environ.get("PLOY_DATABASE__URL")
    or os.environ.get("DATABASE_URL")
    or "postgresql://postgres:postgres@localhost:5432/ploy"
)

SIMULATED_RUNTIME_MODES = ("dry_run", "dryrun", "paper")
MODE_FILTER = ",".join(f"'{mode}'" for mode in SIMULATED_RUNTIME_MODES)


EVENTS_QUERY = f"""
WITH events AS (
  SELECT
    t.runtime_mode,
    t.strategy_id,
    t.deployment_id,
    t.trade_key,
    t.event_id,
    t.intent_id,
    t.symbol,
    t.token_id,
    t.market_side,
    t.opened_at,
    t.closed_at,
    t.last_fill_at,
    t.fill_count,
    t.buy_quantity,
    t.buy_notional,
    t.total_fee,
    t.avg_entry_price,
    t.avg_exit_price,
    t.gross_pnl,
    t.net_pnl,
    t.is_closed,
    t.open_quantity,
    CASE
      WHEN m.end_time IS NOT NULL AND m.start_time IS NOT NULL
        THEN ROUND(EXTRACT(EPOCH FROM (m.end_time - m.start_time)))::int
      WHEN m.market_slug ILIKE '%15m%' OR m.market_slug ILIKE '%15-minute%' THEN 900
      WHEN m.market_slug ILIKE '%5m%' OR m.market_slug ILIKE '%5-minute%' THEN 300
      ELSE NULL
    END AS window_secs,
    CASE
      WHEN m.end_time IS NOT NULL AND t.opened_at IS NOT NULL
        THEN ROUND(EXTRACT(EPOCH FROM (m.end_time - t.opened_at)))::int
      ELSE NULL
    END AS entry_time_remaining_secs
  FROM strategy_runtime_event_track_record t
  LEFT JOIN pm_market_metadata m ON m.market_slug = t.event_id
  WHERE t.runtime_mode IN ({MODE_FILTER})
)
SELECT COALESCE(json_agg(row_to_json(e) ORDER BY e.opened_at, e.trade_key), '[]'::json)::text
FROM events e;
"""

DAILY_QUERY = f"""
SELECT COALESCE(json_agg(row_to_json(d) ORDER BY d.trading_day_cst, d.runtime_mode, d.strategy_id, d.deployment_id), '[]'::json)::text
FROM (
  SELECT
    runtime_mode,
    strategy_id,
    deployment_id,
    trading_day_cst,
    trade_count,
    closed_trade_count,
    winning_trade_count_all AS wins,
    losing_trade_count_all AS losses,
    confirmed_trade_count,
    net_pnl,
    confirmed_net_pnl,
    total_fee AS fees,
    residual_open_quantity AS open_quantity
  FROM strategy_runtime_daily_track_record
  WHERE runtime_mode IN ({MODE_FILTER})
) d;
"""

PAIRING_QUERY = f"""
SELECT json_build_object(
  'pair_key', 'runtime_mode,strategy_id,deployment_id,event_id',
  'mixed_event_groups', COUNT(*),
  'fills_in_mixed_event_groups', COALESCE(SUM(fill_count), 0),
  'current_view_rows', (
    SELECT COUNT(*)
    FROM strategy_runtime_event_track_record
    WHERE runtime_mode IN ({MODE_FILTER})
  ),
  'side_aware_rows', (
    SELECT COUNT(*)
    FROM (
      SELECT
        runtime_mode,
        strategy_id,
        deployment_id,
        COALESCE(NULLIF(event_id, ''), intent_id) AS event_or_intent,
        COALESCE(NULLIF(token_id, ''), 'unknown') AS token_key,
        COALESCE(NULLIF(market_side, ''), 'unknown') AS side_key
      FROM strategy_runtime_fills
      WHERE runtime_mode IN ({MODE_FILTER})
      GROUP BY runtime_mode, strategy_id, deployment_id, event_or_intent, token_key, side_key
    ) side_groups
  )
)::text AS payload
FROM (
  SELECT
    runtime_mode,
    strategy_id,
    deployment_id,
    event_id,
    COUNT(*) AS fill_count
  FROM strategy_runtime_fills
  WHERE runtime_mode IN ({MODE_FILTER})
    AND event_id IS NOT NULL
    AND event_id <> ''
  GROUP BY runtime_mode, strategy_id, deployment_id, event_id
  HAVING COUNT(DISTINCT token_id) > 1 OR COUNT(DISTINCT market_side) > 1
) mixed;
"""


def run_json_query(query: str, timeout: int = 30):
    result = subprocess.run(
        [
            "psql",
            DB_URL,
            "-t",
            "-A",
            "-v",
            "ON_ERROR_STOP=1",
            "-c",
            query,
        ],
        capture_output=True,
        text=True,
        timeout=timeout,
    )
    if result.returncode != 0:
        sys.stderr.write(result.stderr or result.stdout)
        raise SystemExit(result.returncode)
    payload = result.stdout.strip()
    if not payload:
        return None
    return json.loads(payload)


def number(value, default=0.0) -> float:
    if value is None:
        return default
    try:
        parsed = float(value)
    except (TypeError, ValueError):
        return default
    if not math.isfinite(parsed):
        return default
    return parsed


def rounded(value, digits=2):
    return round(number(value), digits)


def strategy_key(row) -> tuple[str, str, str]:
    return (
        row.get("runtime_mode") or "",
        row.get("strategy_id") or "",
        row.get("deployment_id") or "",
    )


def strategy_label(runtime_mode: str, strategy_id: str, deployment_id: str) -> str:
    if deployment_id:
        return deployment_id
    if strategy_id:
        return strategy_id
    return runtime_mode or "unknown"


def window_label(window_secs) -> str:
    if window_secs == 300:
        return "5m"
    if window_secs == 900:
        return "15m"
    if window_secs is None:
        return "unknown"
    return f"{window_secs}s"


def sort_timestamp(value):
    return value or ""


def day_from_event(row) -> str | None:
    timestamp = row.get("closed_at") or row.get("last_fill_at") or row.get("opened_at")
    if not timestamp:
        return None
    return timestamp[:10]


def win_rate(wins: int, closed_count: int) -> float:
    if closed_count <= 0:
        return 0.0
    return round(wins / closed_count * 100, 1)


def build_summary(events):
    closed = [event for event in events if event.get("is_closed")]
    open_events = [event for event in events if not event.get("is_closed")]
    wins = sum(1 for event in closed if number(event.get("net_pnl")) > 0)
    losses = sum(1 for event in closed if number(event.get("net_pnl")) <= 0)
    latest_opened_at = max((event.get("opened_at") for event in events if event.get("opened_at")), default=None)
    latest_closed_at = max((event.get("closed_at") for event in closed if event.get("closed_at")), default=None)
    return {
        "total_trades": len(events),
        "closed_trades": len(closed),
        "wins": wins,
        "losses": losses,
        "win_rate_pct": win_rate(wins, len(closed)),
        "realized_pnl": rounded(sum(number(event.get("net_pnl")) for event in closed)),
        "total_fees": rounded(sum(number(event.get("total_fee")) for event in closed)),
        "open_positions": len(open_events),
        "open_exposure": rounded(sum(number(event.get("buy_notional")) for event in open_events)),
        "latest_opened_at": latest_opened_at,
        "latest_closed_at": latest_closed_at,
    }


def build_equity_curve(events):
    cumulative = 0.0
    peak = 0.0
    points = []
    closed = sorted(
        (event for event in events if event.get("is_closed")),
        key=lambda event: (sort_timestamp(event.get("closed_at")), event.get("trade_key") or ""),
    )
    for index, event in enumerate(closed, start=1):
        pnl = number(event.get("net_pnl"))
        cumulative += pnl
        peak = max(peak, cumulative)
        points.append(
            {
                "index": index,
                "label": str(index),
                "timestamp": event.get("closed_at"),
                "symbol": event.get("symbol"),
                "pnl": round(pnl, 4),
                "cumulative": round(cumulative, 4),
                "drawdown": round(cumulative - peak, 4),
            }
        )
    return points


def build_metrics(events, equity_curve):
    closed_pnls = [number(event.get("net_pnl")) for event in events if event.get("is_closed")]
    gross_profit = sum(pnl for pnl in closed_pnls if pnl > 0)
    gross_loss = abs(sum(pnl for pnl in closed_pnls if pnl < 0))
    profit_factor = None
    if gross_loss > 0:
        profit_factor = round(gross_profit / gross_loss, 4)
    elif gross_profit > 0:
        profit_factor = "Infinity"

    sharpe = None
    if len(closed_pnls) >= 2:
        sigma = stdev(closed_pnls)
        if sigma > 0:
            sharpe = round((mean(closed_pnls) / sigma) * math.sqrt(len(closed_pnls)), 4)

    max_drawdown = min((point["drawdown"] for point in equity_curve), default=0.0)
    avg_trade = mean(closed_pnls) if closed_pnls else None
    return {
        "sharpe": sharpe,
        "profit_factor": profit_factor,
        "max_drawdown": round(max_drawdown, 4),
        "avg_trade": None if avg_trade is None else round(avg_trade, 4),
        "gross_profit": round(gross_profit, 4),
        "gross_loss": round(gross_loss, 4),
        "equity_points": len(equity_curve),
    }


def build_window_rows(events):
    grouped = defaultdict(list)
    for event in events:
        grouped[event.get("window_secs")].append(event)
    rows = []
    for window_secs, window_events in grouped.items():
        closed = [event for event in window_events if event.get("is_closed")]
        wins = sum(1 for event in closed if number(event.get("net_pnl")) > 0)
        losses = sum(1 for event in closed if number(event.get("net_pnl")) <= 0)
        entry_values = [number(event.get("avg_entry_price")) for event in closed if event.get("avg_entry_price") is not None]
        ttr_values = [
            int(event.get("entry_time_remaining_secs"))
            for event in window_events
            if event.get("entry_time_remaining_secs") is not None
        ]
        rows.append(
            {
                "window_secs": window_secs,
                "window_label": window_label(window_secs),
                "total_trades": len(window_events),
                "closed_trades": len(closed),
                "wins": wins,
                "losses": losses,
                "win_rate_pct": win_rate(wins, len(closed)),
                "realized_pnl": rounded(sum(number(event.get("net_pnl")) for event in closed)),
                "avg_pnl": None if not closed else rounded(mean(number(event.get("net_pnl")) for event in closed)),
                "avg_entry": None if not entry_values else round(mean(entry_values), 4),
                "min_entry_ttr_secs": min(ttr_values) if ttr_values else None,
                "max_entry_ttr_secs": max(ttr_values) if ttr_values else None,
            }
        )
    return sorted(rows, key=lambda row: (row["window_secs"] is None, row["window_secs"] or 0))


def build_daily_rows(daily_rows):
    grouped = defaultdict(list)
    for row in daily_rows:
        grouped[row.get("trading_day_cst")].append(row)
    rows = []
    for trading_day, day_rows in grouped.items():
        rows.append(
            {
                "trading_day_cst": trading_day,
                "trade_count": int(sum(number(row.get("trade_count")) for row in day_rows)),
                "closed_trade_count": int(sum(number(row.get("closed_trade_count")) for row in day_rows)),
                "wins": int(sum(number(row.get("wins")) for row in day_rows)),
                "losses": int(sum(number(row.get("losses")) for row in day_rows)),
                "confirmed_trade_count": int(sum(number(row.get("confirmed_trade_count")) for row in day_rows)),
                "net_pnl": rounded(sum(number(row.get("net_pnl")) for row in day_rows)),
                "confirmed_pnl": rounded(sum(number(row.get("confirmed_net_pnl")) for row in day_rows)),
                "fees": rounded(sum(number(row.get("fees")) for row in day_rows)),
                "open_quantity": rounded(sum(number(row.get("open_quantity")) for row in day_rows), 4),
            }
        )
    return sorted(rows, key=lambda row: row["trading_day_cst"], reverse=True)


def build_daily_by_window(events):
    grouped = defaultdict(list)
    for event in events:
        day = day_from_event(event)
        if day is not None:
            grouped[(day, event.get("window_secs"))].append(event)
    rows = []
    for (trading_day, window_secs), window_events in grouped.items():
        closed = [event for event in window_events if event.get("is_closed")]
        rows.append(
            {
                "trading_day_cst": trading_day,
                "window_secs": window_secs,
                "window_label": window_label(window_secs),
                "trade_count": len(window_events),
                "closed_trade_count": len(closed),
                "wins": sum(1 for event in closed if number(event.get("net_pnl")) > 0),
                "losses": sum(1 for event in closed if number(event.get("net_pnl")) <= 0),
                "net_pnl": rounded(sum(number(event.get("net_pnl")) for event in closed)),
            }
        )
    return sorted(rows, key=lambda row: (row["trading_day_cst"], row["window_secs"] or 0), reverse=True)


def build_symbol_rows(events, include_window=False):
    grouped = defaultdict(list)
    for event in events:
        key = (event.get("symbol") or "unknown", event.get("window_secs") if include_window else None)
        grouped[key].append(event)
    rows = []
    for (symbol, window_secs), symbol_events in grouped.items():
        closed = [event for event in symbol_events if event.get("is_closed")]
        entry_values = [number(event.get("avg_entry_price")) for event in closed if event.get("avg_entry_price") is not None]
        row = {
            "symbol": symbol,
            "trades": len(symbol_events),
            "wins": sum(1 for event in closed if number(event.get("net_pnl")) > 0),
            "losses": sum(1 for event in closed if number(event.get("net_pnl")) <= 0),
            "net_pnl": rounded(sum(number(event.get("net_pnl")) for event in closed)),
            "avg_entry": None if not entry_values else round(mean(entry_values), 4),
        }
        if include_window:
            row["window_secs"] = window_secs
            row["window_label"] = window_label(window_secs)
        rows.append(row)
    return sorted(rows, key=lambda row: number(row["net_pnl"]), reverse=True)


def exit_type(event):
    exit_price = number(event.get("avg_exit_price"), default=math.nan)
    if math.isfinite(exit_price):
        if exit_price >= 0.99:
            return "WIN"
        if exit_price <= 0.01:
            return "LOSS"
    return "TP/SL"


def closed_trade_row(event):
    window_secs = event.get("window_secs")
    return {
        "runtime_mode": event.get("runtime_mode"),
        "strategy_id": event.get("strategy_id"),
        "deployment_id": event.get("deployment_id"),
        "trade_key": event.get("trade_key"),
        "event_id": event.get("event_id"),
        "symbol": event.get("symbol"),
        "window_secs": window_secs,
        "window_label": window_label(window_secs),
        "market_side": event.get("market_side"),
        "entry_price": None if event.get("avg_entry_price") is None else round(number(event.get("avg_entry_price")), 4),
        "exit_price": None if event.get("avg_exit_price") is None else round(number(event.get("avg_exit_price")), 4),
        "exit_type": exit_type(event),
        "quantity": round(number(event.get("buy_quantity")), 4),
        "notional": round(number(event.get("buy_notional")), 4),
        "net_pnl": round(number(event.get("net_pnl")), 4),
        "entry_time_remaining_secs": event.get("entry_time_remaining_secs"),
        "opened_at": event.get("opened_at"),
        "closed_at": event.get("closed_at"),
    }


def build_closed_trades(events):
    return [
        closed_trade_row(event)
        for event in sorted(
            (event for event in events if event.get("is_closed")),
            key=lambda event: (sort_timestamp(event.get("closed_at")), event.get("trade_key") or ""),
            reverse=True,
        )
    ]


def build_open_positions(events):
    rows = []
    for event in sorted(
        (event for event in events if not event.get("is_closed")),
        key=lambda event: (sort_timestamp(event.get("opened_at")), event.get("trade_key") or ""),
        reverse=True,
    ):
        window_secs = event.get("window_secs")
        rows.append(
            {
                "runtime_mode": event.get("runtime_mode"),
                "strategy_id": event.get("strategy_id"),
                "deployment_id": event.get("deployment_id"),
                "trade_key": event.get("trade_key"),
                "event_id": event.get("event_id"),
                "symbol": event.get("symbol"),
                "window_secs": window_secs,
                "window_label": window_label(window_secs),
                "market_side": event.get("market_side"),
                "entry_price": None if event.get("avg_entry_price") is None else round(number(event.get("avg_entry_price")), 4),
                "quantity": round(number(event.get("buy_quantity")), 4),
                "notional": round(number(event.get("buy_notional")), 4),
                "entry_time_remaining_secs": event.get("entry_time_remaining_secs"),
                "opened_at": event.get("opened_at"),
            }
        )
    return rows


def build_report_slice(events, daily_rows):
    equity_curve = build_equity_curve(events)
    closed_trades = build_closed_trades(events)
    return {
        "summary": build_summary(events),
        "metrics": build_metrics(events, equity_curve),
        "equity_curve": equity_curve,
        "by_window": build_window_rows(events),
        "daily": build_daily_rows(daily_rows),
        "daily_by_window": build_daily_by_window(events),
        "symbols": build_symbol_rows(events),
        "symbols_by_window": build_symbol_rows(events, include_window=True),
        "closed_trades": closed_trades[:250],
        "recent_closed": closed_trades[:50],
        "open_positions": build_open_positions(events),
    }


def empty_payload():
    now = datetime.now(timezone.utc).isoformat()
    return {
        "generated_at": now,
        "summary": build_summary([]),
        "metrics": build_metrics([], []),
        "equity_curve": [],
        "by_window": [],
        "daily": [],
        "daily_by_window": [],
        "symbols": [],
        "symbols_by_window": [],
        "closed_trades": [],
        "recent_closed": [],
        "open_positions": [],
        "strategies": [],
        "pairing": {
            "pair_key": "runtime_mode,strategy_id,deployment_id,event_id",
            "mixed_event_groups": 0,
            "fills_in_mixed_event_groups": 0,
            "current_view_rows": 0,
            "side_aware_rows": 0,
        },
    }


def main() -> int:
    events = run_json_query(EVENTS_QUERY) or []
    daily_rows = run_json_query(DAILY_QUERY) or []
    pairing = run_json_query(PAIRING_QUERY) or empty_payload()["pairing"]

    payload = empty_payload()
    payload["generated_at"] = datetime.now(timezone.utc).isoformat()
    payload.update(build_report_slice(events, daily_rows))
    payload["pairing"] = pairing

    events_by_strategy = defaultdict(list)
    for event in events:
        events_by_strategy[strategy_key(event)].append(event)

    daily_by_strategy = defaultdict(list)
    for row in daily_rows:
        daily_by_strategy[strategy_key(row)].append(row)

    strategies = []
    for runtime_mode, strategy_id, deployment_id in sorted(events_by_strategy.keys()):
        strategy_events = events_by_strategy[(runtime_mode, strategy_id, deployment_id)]
        strategy_daily_rows = daily_by_strategy.get((runtime_mode, strategy_id, deployment_id), [])
        strategy_payload = build_report_slice(strategy_events, strategy_daily_rows)
        strategy_payload.update(
            {
                "runtime_mode": runtime_mode,
                "strategy_id": strategy_id,
                "deployment_id": deployment_id,
                "label": strategy_label(runtime_mode, strategy_id, deployment_id),
            }
        )
        strategies.append(strategy_payload)

    payload["strategies"] = strategies
    print(json.dumps(payload, separators=(",", ":")))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
