#!/usr/bin/env python3
"""Emit dry-run strategy performance summary JSON from the local Ploy database."""

import json
import os
import subprocess
import sys
from datetime import datetime, timezone


DB_URL = (
    os.environ.get("PLOY_DATABASE__URL")
    or os.environ.get("DATABASE_URL")
    or "postgresql://postgres:postgres@localhost:5432/ploy"
)

QUERY = r"""
WITH events AS (
  SELECT *
  FROM strategy_runtime_event_track_record
  WHERE runtime_mode = 'dry_run'
),
summary AS (
  SELECT json_build_object(
    'total_trades', COUNT(*),
    'closed_trades', SUM(CASE WHEN is_closed THEN 1 ELSE 0 END),
    'wins', SUM(CASE WHEN is_closed AND net_pnl > 0 THEN 1 ELSE 0 END),
    'losses', SUM(CASE WHEN is_closed AND net_pnl <= 0 THEN 1 ELSE 0 END),
    'win_rate_pct', ROUND(
      CASE WHEN SUM(CASE WHEN is_closed THEN 1 ELSE 0 END) > 0
        THEN SUM(CASE WHEN is_closed AND net_pnl > 0 THEN 1 ELSE 0 END)::numeric
          / SUM(CASE WHEN is_closed THEN 1 ELSE 0 END)::numeric * 100
        ELSE 0
      END,
      1
    ),
    'realized_pnl', ROUND(SUM(CASE WHEN is_closed THEN net_pnl ELSE 0 END)::numeric, 2),
    'total_fees', ROUND(SUM(CASE WHEN is_closed THEN total_fee ELSE 0 END)::numeric, 2),
    'open_positions', SUM(CASE WHEN NOT is_closed THEN 1 ELSE 0 END),
    'open_exposure', ROUND(SUM(CASE WHEN NOT is_closed THEN buy_notional ELSE 0 END)::numeric, 2),
    'latest_opened_at', MAX(opened_at),
    'latest_closed_at', MAX(closed_at)
  ) AS payload
  FROM events
),
daily AS (
  SELECT COALESCE(json_agg(row_to_json(d) ORDER BY d.trading_day_cst DESC), '[]'::json) AS payload
  FROM (
    SELECT
      trading_day_cst,
      trade_count,
      closed_trade_count,
      winning_trade_count AS wins,
      losing_trade_count AS losses,
      ROUND(net_pnl::numeric, 2) AS net_pnl,
      ROUND(confirmed_net_pnl::numeric, 2) AS confirmed_pnl,
      ROUND(total_fee::numeric, 2) AS fees,
      residual_open_quantity AS open_quantity
    FROM strategy_runtime_daily_track_record
    WHERE runtime_mode = 'dry_run'
    ORDER BY trading_day_cst DESC
    LIMIT 14
  ) d
),
symbols AS (
  SELECT COALESCE(json_agg(row_to_json(s) ORDER BY s.net_pnl DESC), '[]'::json) AS payload
  FROM (
    SELECT
      symbol,
      COUNT(*) AS trades,
      SUM(CASE WHEN is_closed AND net_pnl > 0 THEN 1 ELSE 0 END) AS wins,
      SUM(CASE WHEN is_closed AND net_pnl <= 0 THEN 1 ELSE 0 END) AS losses,
      ROUND(SUM(CASE WHEN is_closed THEN net_pnl ELSE 0 END)::numeric, 2) AS net_pnl,
      ROUND(AVG(CASE WHEN is_closed THEN avg_entry_price END)::numeric, 4) AS avg_entry
    FROM events
    GROUP BY symbol
    ORDER BY net_pnl DESC
  ) s
),
recent_closed AS (
  SELECT COALESCE(json_agg(row_to_json(t) ORDER BY t.closed_at DESC), '[]'::json) AS payload
  FROM (
    SELECT
      symbol,
      market_side,
      ROUND(avg_entry_price::numeric, 4) AS entry_price,
      ROUND(avg_exit_price::numeric, 4) AS exit_price,
      CASE
        WHEN avg_exit_price >= 0.99 THEN 'WIN'
        WHEN avg_exit_price <= 0.01 THEN 'LOSS'
        ELSE 'TP/SL'
      END AS exit_type,
      ROUND(buy_quantity::numeric, 2) AS quantity,
      ROUND(net_pnl::numeric, 2) AS net_pnl,
      opened_at,
      closed_at
    FROM events
    WHERE is_closed
    ORDER BY closed_at DESC
    LIMIT 20
  ) t
),
open_positions AS (
  SELECT COALESCE(json_agg(row_to_json(p) ORDER BY p.opened_at DESC), '[]'::json) AS payload
  FROM (
    SELECT
      symbol,
      market_side,
      ROUND(avg_entry_price::numeric, 4) AS entry_price,
      ROUND(buy_quantity::numeric, 2) AS quantity,
      ROUND(buy_notional::numeric, 2) AS notional,
      opened_at
    FROM events
    WHERE NOT is_closed
    ORDER BY opened_at DESC
    LIMIT 20
  ) p
)
SELECT json_build_object(
  'generated_at', NOW(),
  'summary', (SELECT payload FROM summary),
  'daily', (SELECT payload FROM daily),
  'symbols', (SELECT payload FROM symbols),
  'recent_closed', (SELECT payload FROM recent_closed),
  'open_positions', (SELECT payload FROM open_positions)
)::text;
"""


def main() -> int:
    result = subprocess.run(
        [
            "psql",
            DB_URL,
            "-t",
            "-A",
            "-v",
            "ON_ERROR_STOP=1",
            "-c",
            QUERY,
        ],
        capture_output=True,
        text=True,
        timeout=15,
    )
    if result.returncode != 0:
        sys.stderr.write(result.stderr or result.stdout)
        return result.returncode

    payload = result.stdout.strip()
    if not payload:
        print(
            json.dumps(
                {
                    "generated_at": datetime.now(timezone.utc).isoformat(),
                    "summary": {
                        "total_trades": 0,
                        "closed_trades": 0,
                        "wins": 0,
                        "losses": 0,
                        "win_rate_pct": 0,
                        "realized_pnl": 0,
                        "total_fees": 0,
                        "open_positions": 0,
                        "open_exposure": 0,
                        "latest_opened_at": None,
                        "latest_closed_at": None,
                    },
                    "daily": [],
                    "symbols": [],
                    "recent_closed": [],
                    "open_positions": [],
                },
                separators=(",", ":"),
            )
        )
        return 0

    print(json.dumps(json.loads(payload), separators=(",", ":")))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
