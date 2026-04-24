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
  SELECT
    t.*,
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
  WHERE t.runtime_mode = 'dry_run'
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
by_window AS (
  SELECT COALESCE(json_agg(row_to_json(w) ORDER BY w.window_secs), '[]'::json) AS payload
  FROM (
    SELECT
      window_secs,
      CASE
        WHEN window_secs = 300 THEN '5m'
        WHEN window_secs = 900 THEN '15m'
        WHEN window_secs IS NULL THEN 'unknown'
        ELSE (window_secs::text || 's')
      END AS window_label,
      COUNT(*) AS total_trades,
      SUM(CASE WHEN is_closed THEN 1 ELSE 0 END) AS closed_trades,
      SUM(CASE WHEN is_closed AND net_pnl > 0 THEN 1 ELSE 0 END) AS wins,
      SUM(CASE WHEN is_closed AND net_pnl <= 0 THEN 1 ELSE 0 END) AS losses,
      ROUND(
        CASE WHEN SUM(CASE WHEN is_closed THEN 1 ELSE 0 END) > 0
          THEN SUM(CASE WHEN is_closed AND net_pnl > 0 THEN 1 ELSE 0 END)::numeric
            / SUM(CASE WHEN is_closed THEN 1 ELSE 0 END)::numeric * 100
          ELSE 0
        END,
        1
      ) AS win_rate_pct,
      ROUND(SUM(CASE WHEN is_closed THEN net_pnl ELSE 0 END)::numeric, 2) AS realized_pnl,
      ROUND(AVG(CASE WHEN is_closed THEN net_pnl END)::numeric, 2) AS avg_pnl,
      ROUND(AVG(CASE WHEN is_closed THEN avg_entry_price END)::numeric, 4) AS avg_entry,
      MIN(entry_time_remaining_secs) AS min_entry_ttr_secs,
      MAX(entry_time_remaining_secs) AS max_entry_ttr_secs
    FROM events
    GROUP BY window_secs
  ) w
),
daily AS (
  SELECT COALESCE(json_agg(row_to_json(d) ORDER BY d.trading_day_cst DESC), '[]'::json) AS payload
  FROM (
    SELECT
      trading_day_cst,
      trade_count,
      closed_trade_count,
      winning_trade_count_all AS wins,
      losing_trade_count_all AS losses,
      confirmed_trade_count,
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
daily_by_window AS (
  SELECT COALESCE(json_agg(row_to_json(d) ORDER BY d.trading_day_cst DESC, d.window_secs), '[]'::json) AS payload
  FROM (
    SELECT
      (COALESCE(closed_at, last_fill_at) AT TIME ZONE 'Asia/Shanghai')::date AS trading_day_cst,
      window_secs,
      CASE
        WHEN window_secs = 300 THEN '5m'
        WHEN window_secs = 900 THEN '15m'
        WHEN window_secs IS NULL THEN 'unknown'
        ELSE (window_secs::text || 's')
      END AS window_label,
      COUNT(*) AS trade_count,
      SUM(CASE WHEN is_closed THEN 1 ELSE 0 END) AS closed_trade_count,
      SUM(CASE WHEN is_closed AND net_pnl > 0 THEN 1 ELSE 0 END) AS wins,
      SUM(CASE WHEN is_closed AND net_pnl <= 0 THEN 1 ELSE 0 END) AS losses,
      ROUND(SUM(CASE WHEN is_closed THEN net_pnl ELSE 0 END)::numeric, 2) AS net_pnl
    FROM events
    GROUP BY
      (COALESCE(closed_at, last_fill_at) AT TIME ZONE 'Asia/Shanghai')::date,
      window_secs
    ORDER BY trading_day_cst DESC, window_secs
    LIMIT 28
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
symbols_by_window AS (
  SELECT COALESCE(json_agg(row_to_json(s) ORDER BY s.window_secs, s.net_pnl DESC), '[]'::json) AS payload
  FROM (
    SELECT
      symbol,
      window_secs,
      CASE
        WHEN window_secs = 300 THEN '5m'
        WHEN window_secs = 900 THEN '15m'
        WHEN window_secs IS NULL THEN 'unknown'
        ELSE (window_secs::text || 's')
      END AS window_label,
      COUNT(*) AS trades,
      SUM(CASE WHEN is_closed AND net_pnl > 0 THEN 1 ELSE 0 END) AS wins,
      SUM(CASE WHEN is_closed AND net_pnl <= 0 THEN 1 ELSE 0 END) AS losses,
      ROUND(SUM(CASE WHEN is_closed THEN net_pnl ELSE 0 END)::numeric, 2) AS net_pnl,
      ROUND(AVG(CASE WHEN is_closed THEN avg_entry_price END)::numeric, 4) AS avg_entry
    FROM events
    GROUP BY symbol, window_secs
    ORDER BY window_secs, net_pnl DESC
  ) s
),
recent_closed AS (
  SELECT COALESCE(json_agg(row_to_json(t) ORDER BY t.closed_at DESC), '[]'::json) AS payload
  FROM (
    SELECT
      symbol,
      window_secs,
      CASE
        WHEN window_secs = 300 THEN '5m'
        WHEN window_secs = 900 THEN '15m'
        WHEN window_secs IS NULL THEN 'unknown'
        ELSE (window_secs::text || 's')
      END AS window_label,
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
      entry_time_remaining_secs,
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
      window_secs,
      CASE
        WHEN window_secs = 300 THEN '5m'
        WHEN window_secs = 900 THEN '15m'
        WHEN window_secs IS NULL THEN 'unknown'
        ELSE (window_secs::text || 's')
      END AS window_label,
      market_side,
      ROUND(avg_entry_price::numeric, 4) AS entry_price,
      ROUND(buy_quantity::numeric, 2) AS quantity,
      ROUND(buy_notional::numeric, 2) AS notional,
      entry_time_remaining_secs,
      opened_at
    FROM events
    WHERE NOT is_closed
    ORDER BY opened_at DESC
    LIMIT 20
  ) p
),
pairing AS (
  SELECT json_build_object(
    'pair_key', 'runtime_mode,strategy_id,deployment_id,event_id',
    'mixed_event_groups', COUNT(*),
    'fills_in_mixed_event_groups', COALESCE(SUM(fill_count), 0),
    'current_view_rows', (
      SELECT COUNT(*)
      FROM strategy_runtime_event_track_record
      WHERE runtime_mode = 'dry_run'
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
        WHERE runtime_mode = 'dry_run'
        GROUP BY runtime_mode, strategy_id, deployment_id, event_or_intent, token_key, side_key
      ) side_groups
    )
  ) AS payload
  FROM (
    SELECT
      runtime_mode,
      strategy_id,
      deployment_id,
      event_id,
      COUNT(*) AS fill_count
    FROM strategy_runtime_fills
    WHERE runtime_mode = 'dry_run'
      AND event_id IS NOT NULL
      AND event_id <> ''
    GROUP BY runtime_mode, strategy_id, deployment_id, event_id
    HAVING COUNT(DISTINCT token_id) > 1 OR COUNT(DISTINCT market_side) > 1
  ) mixed
)
SELECT json_build_object(
  'generated_at', NOW(),
  'summary', (SELECT payload FROM summary),
  'by_window', (SELECT payload FROM by_window),
  'daily', (SELECT payload FROM daily),
  'daily_by_window', (SELECT payload FROM daily_by_window),
  'symbols', (SELECT payload FROM symbols),
  'symbols_by_window', (SELECT payload FROM symbols_by_window),
  'recent_closed', (SELECT payload FROM recent_closed),
  'open_positions', (SELECT payload FROM open_positions),
  'pairing', (SELECT payload FROM pairing)
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
                    "by_window": [],
                    "daily_by_window": [],
                    "symbols": [],
                    "symbols_by_window": [],
                    "recent_closed": [],
                    "open_positions": [],
                    "pairing": {
                        "pair_key": "runtime_mode,strategy_id,deployment_id,event_id",
                        "mixed_event_groups": 0,
                        "fills_in_mixed_event_groups": 0,
                        "current_view_rows": 0,
                        "side_aware_rows": 0,
                    },
                },
                separators=(",", ":"),
            )
        )
        return 0

    print(json.dumps(json.loads(payload), separators=(",", ":")))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
