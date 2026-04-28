#!/usr/bin/env python3
"""Audit live market-data coverage with bounded index probes.

This script is intended to run on the data/runtime host next to PostgreSQL. It
uses psql instead of a Python database dependency so it can be shipped with the
existing lightweight deployment bundle.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
import time
from dataclasses import dataclass
from datetime import datetime, timezone
from typing import Any, Iterable, Optional

DB_URL = (
    os.environ.get("PLOY_DATABASE__URL")
    or os.environ.get("DATABASE_URL")
    or "postgresql://postgres:postgres@localhost:5432/ploy"
)

DEFAULT_SYMBOLS = "BTCUSDT,ETHUSDT,SOLUSDT,XRPUSDT,DOGEUSDT,BNBUSDT"
STATUS_ORDER = {"ok": 0, "warn": 1, "unknown": 2, "critical": 3}
IDENT_RE = re.compile(r"^[A-Za-z_][A-Za-z0-9_]*$")


@dataclass(frozen=True)
class GapTarget:
    source_id: str
    table_name: str
    timestamp_column: str
    stale_after_seconds: int
    filter_column: Optional[str] = None
    filter_value: Optional[str] = None


def ident(value: str) -> str:
    if not IDENT_RE.match(value):
        raise ValueError(f"unsafe SQL identifier: {value!r}")
    return value


def sql_literal(value: str) -> str:
    return "'" + value.replace("'", "''") + "'"


def run_sql(query: str, timeout: int) -> str:
    cmd = [
        "psql",
        DB_URL,
        "-q",
        "-t",
        "-A",
        "-v",
        "ON_ERROR_STOP=1",
        "-c",
        query,
    ]
    result = subprocess.run(cmd, capture_output=True, text=True, timeout=timeout)
    if result.returncode != 0:
        raise RuntimeError(result.stderr.strip() or result.stdout.strip())
    return result.stdout.strip()


def run_json(query: str, timeout: int) -> dict[str, Any]:
    raw = run_sql(query, timeout)
    if not raw:
        raise RuntimeError("SQL returned no rows")
    return json.loads(raw)


def target_predicate(target: GapTarget, alias: str = "t") -> str:
    if target.filter_column is None:
        return "TRUE"
    return f"{alias}.{ident(target.filter_column)} = {sql_literal(target.filter_value or '')}"


def gap_query(
    target: GapTarget,
    *,
    lookback_hours: int,
    bucket_minutes: int,
    recent_minutes: int,
    statement_timeout_seconds: int,
) -> str:
    table = ident(target.table_name)
    ts_col = ident(target.timestamp_column)
    predicate = target_predicate(target)
    return f"""
SET statement_timeout TO '{statement_timeout_seconds}s';
WITH params AS (
  SELECT
    date_trunc('minute', now() - interval '{lookback_hours} hours') AS start_at,
    date_trunc('minute', now() - interval '{bucket_minutes} minutes') AS end_at,
    interval '{bucket_minutes} minutes' AS bucket_width
),
buckets AS (
  SELECT generate_series(start_at, end_at, bucket_width) AS bucket
  FROM params
),
coverage AS (
  SELECT b.bucket, hit.present IS NOT NULL AS present
  FROM buckets b
  CROSS JOIN params p
  LEFT JOIN LATERAL (
    SELECT TRUE AS present
    FROM {table} t
    WHERE {predicate}
      AND t.{ts_col} >= b.bucket
      AND t.{ts_col} < b.bucket + p.bucket_width
    ORDER BY t.{ts_col} DESC
    LIMIT 1
  ) hit ON TRUE
),
missing AS (
  SELECT
    bucket,
    bucket - (row_number() OVER (ORDER BY bucket))::int * (SELECT bucket_width FROM params) AS grp
  FROM coverage
  WHERE NOT present
),
runs AS (
  SELECT min(bucket) AS gap_start, max(bucket) AS gap_last_bucket, count(*)::int AS bucket_count
  FROM missing
  GROUP BY grp
),
top_gap AS (
  SELECT gap_start, gap_last_bucket, bucket_count
  FROM runs
  ORDER BY bucket_count DESC, gap_start DESC
  LIMIT 1
),
latest AS (
  SELECT t.{ts_col} AS latest_at
  FROM {table} t
  WHERE {predicate}
  ORDER BY t.{ts_col} DESC
  LIMIT 1
),
recent AS (
  SELECT count(*)::bigint AS rows_recent
  FROM {table} t
  WHERE {predicate}
    AND t.{ts_col} >= now() - interval '{recent_minutes} minutes'
)
SELECT json_build_object(
  'source_id', {sql_literal(target.source_id)},
  'table_name', {sql_literal(target.table_name)},
  'filter_column', {sql_literal(target.filter_column or '')},
  'filter_value', {sql_literal(target.filter_value or '')},
  'timestamp_column', {sql_literal(target.timestamp_column)},
  'lookback_hours', {lookback_hours},
  'bucket_minutes', {bucket_minutes},
  'recent_minutes', {recent_minutes},
  'start_at', (SELECT start_at FROM params),
  'end_at', (SELECT end_at FROM params),
  'latest_at', (SELECT latest_at FROM latest),
  'latest_lag_seconds', (
    SELECT CASE
      WHEN latest_at IS NULL THEN NULL
      ELSE GREATEST(0, floor(extract(epoch FROM now() - latest_at))::bigint)
    END
    FROM latest
  ),
  'rows_recent', (SELECT rows_recent FROM recent),
  'expected_buckets', (SELECT count(*)::int FROM coverage),
  'present_buckets', (SELECT count(*)::int FROM coverage WHERE present),
  'missing_buckets', (SELECT count(*)::int FROM coverage WHERE NOT present),
  'coverage_pct', (
    SELECT CASE
      WHEN count(*) = 0 THEN NULL
      ELSE round((100.0 * count(*) FILTER (WHERE present) / count(*))::numeric, 3)
    END
    FROM coverage
  ),
  'max_gap_buckets', coalesce((SELECT bucket_count FROM top_gap), 0),
  'max_gap_minutes', coalesce((SELECT bucket_count FROM top_gap), 0) * {bucket_minutes},
  'max_gap_start', (SELECT gap_start FROM top_gap),
  'max_gap_end', (SELECT gap_last_bucket + (SELECT bucket_width FROM params) FROM top_gap)
)::text
"""


def classify_gap(row: dict[str, Any], target: GapTarget) -> tuple[str, list[str]]:
    reasons: list[str] = []
    status = "ok"

    latest_at = row.get("latest_at")
    latest_lag = row.get("latest_lag_seconds")
    if latest_at is None:
        return "critical", ["no latest row"]
    if latest_lag is not None and latest_lag > target.stale_after_seconds:
        status = "critical"
        reasons.append(f"latest lag {latest_lag}s > {target.stale_after_seconds}s")

    max_gap_minutes = int(row.get("max_gap_minutes") or 0)
    if max_gap_minutes >= 15:
        status = "critical"
        reasons.append(f"max gap {max_gap_minutes}m >= 15m")
    elif max_gap_minutes >= 5 and STATUS_ORDER[status] < STATUS_ORDER["warn"]:
        status = "warn"
        reasons.append(f"max gap {max_gap_minutes}m >= 5m")

    missing_buckets = int(row.get("missing_buckets") or 0)
    if missing_buckets > 0 and STATUS_ORDER[status] < STATUS_ORDER["warn"]:
        status = "warn"
        reasons.append(f"missing buckets {missing_buckets}")

    if not reasons:
        reasons.append("coverage within thresholds")
    return status, reasons


def audit_gap_target(
    target: GapTarget,
    *,
    lookback_hours: int,
    bucket_minutes: int,
    recent_minutes: int,
    statement_timeout_seconds: int,
    psql_timeout_seconds: int,
) -> dict[str, Any]:
    started = time.monotonic()
    try:
        row = run_json(
            gap_query(
                target,
                lookback_hours=lookback_hours,
                bucket_minutes=bucket_minutes,
                recent_minutes=recent_minutes,
                statement_timeout_seconds=statement_timeout_seconds,
            ),
            psql_timeout_seconds,
        )
        status, reasons = classify_gap(row, target)
        row["status"] = status
        row["reasons"] = reasons
    except Exception as exc:  # noqa: BLE001 - this is an operator diagnostic.
        row = {
            "source_id": target.source_id,
            "table_name": target.table_name,
            "filter_column": target.filter_column or "",
            "filter_value": target.filter_value or "",
            "timestamp_column": target.timestamp_column,
            "lookback_hours": lookback_hours,
            "bucket_minutes": bucket_minutes,
            "status": "unknown",
            "reasons": [str(exc)],
        }
    row["query_ms"] = round((time.monotonic() - started) * 1000)
    return row


def research_windows_query(statement_timeout_seconds: int) -> str:
    return f"""
SET statement_timeout TO '{statement_timeout_seconds}s';
SELECT json_build_object(
  'source_id', 'research_valid_windows',
  'table_name', 'research_valid_windows',
  'timestamp_column', 'end_time',
  'total_rows', count(*)::bigint,
  'rows_7d', count(*) FILTER (WHERE end_time >= now() - interval '7 days')::bigint,
  'latest_end_time', max(end_time),
  'latest_lag_seconds', CASE
    WHEN max(end_time) IS NULL THEN NULL
    ELSE GREATEST(0, floor(extract(epoch FROM now() - max(end_time)))::bigint)
  END
)::text
FROM research_valid_windows
"""


def audit_research_windows(statement_timeout_seconds: int, psql_timeout_seconds: int) -> dict[str, Any]:
    started = time.monotonic()
    try:
        row = run_json(research_windows_query(statement_timeout_seconds), psql_timeout_seconds)
        reasons: list[str] = []
        status = "ok"
        if int(row.get("rows_7d") or 0) == 0:
            status = "critical"
            reasons.append("no valid windows in last 7 days")
        latest_lag = row.get("latest_lag_seconds")
        if latest_lag is None:
            status = "critical"
            reasons.append("no latest end_time")
        elif latest_lag > 12 * 3600:
            status = "critical"
            reasons.append(f"latest window lag {latest_lag}s > 12h")
        elif latest_lag > 2 * 3600 and STATUS_ORDER[status] < STATUS_ORDER["warn"]:
            status = "warn"
            reasons.append(f"latest window lag {latest_lag}s > 2h")
        if not reasons:
            reasons.append("valid windows are populated and recent")
        row["status"] = status
        row["reasons"] = reasons
    except Exception as exc:  # noqa: BLE001 - this is an operator diagnostic.
        row = {
            "source_id": "research_valid_windows",
            "table_name": "research_valid_windows",
            "status": "unknown",
            "reasons": [str(exc)],
        }
    row["query_ms"] = round((time.monotonic() - started) * 1000)
    return row


def parse_symbols(raw: str) -> list[str]:
    return [item.strip().upper() for item in raw.split(",") if item.strip()]


def gap_targets(symbols: Iterable[str]) -> list[GapTarget]:
    targets = [
        GapTarget("polymarket_quotes", "clob_quote_ticks", "received_at", 300),
        GapTarget("deribit_iv", "deribit_iv_ticks", "fetched_at", 900),
        GapTarget("deribit_atm_greeks", "deribit_atm_greeks_ticks", "fetched_at", 900),
    ]
    for symbol in symbols:
        targets.extend(
            [
                GapTarget(
                    f"binance_price/{symbol}",
                    "binance_price_ticks",
                    "trade_time",
                    600,
                    "symbol",
                    symbol,
                ),
                GapTarget(
                    f"binance_agg_trades/{symbol}",
                    "binance_agg_trade_ticks",
                    "trade_time",
                    600,
                    "symbol",
                    symbol,
                ),
                GapTarget(
                    f"binance_lob/{symbol}",
                    "binance_lob_ticks",
                    "event_time",
                    900,
                    "symbol",
                    symbol,
                ),
            ]
        )
    return targets


def overall_status(items: Iterable[dict[str, Any]]) -> str:
    status = "ok"
    for item in items:
        candidate = str(item.get("status") or "unknown")
        if STATUS_ORDER[candidate] > STATUS_ORDER[status]:
            status = candidate
    return status


def print_text(payload: dict[str, Any]) -> None:
    print("Market Data Gap Audit")
    print(f"generated_at={payload['generated_at']}")
    print(
        f"overall_status={payload['overall_status']} "
        f"lookback_hours={payload['lookback_hours']} "
        f"bucket_minutes={payload['bucket_minutes']}"
    )
    print()
    for item in payload["gap_audits"]:
        filter_value = item.get("filter_value")
        label = item["source_id"] if not filter_value else f"{item['source_id']}"
        latest = item.get("latest_at") or "none"
        lag = item.get("latest_lag_seconds")
        missing = item.get("missing_buckets", "n/a")
        expected = item.get("expected_buckets", "n/a")
        coverage = item.get("coverage_pct", "n/a")
        max_gap = item.get("max_gap_minutes", "n/a")
        print(
            f"[{item['status'].upper()}] {label} "
            f"latest={latest} lag_s={lag} "
            f"coverage={coverage}% missing={missing}/{expected} "
            f"max_gap_m={max_gap} query_ms={item.get('query_ms')}"
        )
        print(f"  reasons: {'; '.join(item.get('reasons') or [])}")
        if item.get("max_gap_start"):
            print(f"  max_gap: {item['max_gap_start']} -> {item['max_gap_end']}")

    print()
    for item in payload["window_audits"]:
        print(
            f"[{item['status'].upper()}] {item['source_id']} "
            f"rows_7d={item.get('rows_7d')} total_rows={item.get('total_rows')} "
            f"latest_end={item.get('latest_end_time')} "
            f"lag_s={item.get('latest_lag_seconds')} query_ms={item.get('query_ms')}"
        )
        print(f"  reasons: {'; '.join(item.get('reasons') or [])}")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--lookback-hours", type=int, default=168)
    parser.add_argument("--bucket-minutes", type=int, default=5)
    parser.add_argument("--recent-minutes", type=int, default=15)
    parser.add_argument("--statement-timeout-seconds", type=int, default=20)
    parser.add_argument("--psql-timeout-seconds", type=int, default=30)
    parser.add_argument(
        "--symbols",
        default=os.environ.get("PLOY_AUDIT_SYMBOLS", DEFAULT_SYMBOLS),
        help=f"comma-separated Binance symbols (default: {DEFAULT_SYMBOLS})",
    )
    parser.add_argument("--format", choices=("text", "json"), default="text")
    parser.add_argument(
        "--fail-on",
        choices=("critical", "unknown", "never"),
        default="critical",
        help="exit non-zero on critical, unknown-or-critical, or never",
    )
    args = parser.parse_args()

    if args.lookback_hours <= 0:
        parser.error("--lookback-hours must be positive")
    if args.bucket_minutes <= 0:
        parser.error("--bucket-minutes must be positive")
    if args.recent_minutes <= 0:
        parser.error("--recent-minutes must be positive")

    symbols = parse_symbols(args.symbols)
    gap_results = [
        audit_gap_target(
            target,
            lookback_hours=args.lookback_hours,
            bucket_minutes=args.bucket_minutes,
            recent_minutes=args.recent_minutes,
            statement_timeout_seconds=args.statement_timeout_seconds,
            psql_timeout_seconds=args.psql_timeout_seconds,
        )
        for target in gap_targets(symbols)
    ]
    window_results = [
        audit_research_windows(args.statement_timeout_seconds, args.psql_timeout_seconds)
    ]
    items = gap_results + window_results
    payload = {
        "generated_at": datetime.now(timezone.utc).isoformat(),
        "database_url_source": "PLOY_DATABASE__URL"
        if os.environ.get("PLOY_DATABASE__URL")
        else ("DATABASE_URL" if os.environ.get("DATABASE_URL") else "default-local"),
        "lookback_hours": args.lookback_hours,
        "bucket_minutes": args.bucket_minutes,
        "recent_minutes": args.recent_minutes,
        "symbols": symbols,
        "overall_status": overall_status(items),
        "gap_audits": gap_results,
        "window_audits": window_results,
    }

    if args.format == "json":
        print(json.dumps(payload, separators=(",", ":"), default=str))
    else:
        print_text(payload)

    if args.fail_on == "never":
        return 0
    if payload["overall_status"] == "critical":
        return 1
    if args.fail_on == "unknown" and payload["overall_status"] in {"critical", "unknown"}:
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
