#!/usr/bin/env python3
"""Backfill official Polymarket settlement rows for bounded PM5D windows."""

from __future__ import annotations

import argparse
import asyncio
import json
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

import asyncpg
import httpx


DEFAULT_SYMBOLS = "BTCUSDT,ETHUSDT,SOLUSDT,XRPUSDT,DOGEUSDT,HYPEUSDT,BNBUSDT"


def parse_symbols(raw: str) -> list[str]:
    return [item.strip().upper() for item in raw.split(",") if item.strip()]


def parse_utc_ts(raw: str) -> datetime | None:
    if not raw:
        return None
    parsed = datetime.fromisoformat(raw.replace("Z", "+00:00"))
    if parsed.tzinfo is None:
        parsed = parsed.replace(tzinfo=timezone.utc)
    return parsed.astimezone(timezone.utc)


def parser() -> argparse.ArgumentParser:
    parsed = argparse.ArgumentParser()
    parsed.add_argument("legacy_db_url", nargs="?", default="")
    parsed.add_argument("--db-url", default="")
    parsed.add_argument("--start-ts", default="2026-04-05T00:00:00Z")
    parsed.add_argument("--end-ts", default="")
    parsed.add_argument("--symbols", default=DEFAULT_SYMBOLS)
    parsed.add_argument("--dry-run", action="store_true")
    parsed.add_argument("--report-json", type=Path)
    return parsed


async def load_candidate_markets(
    conn: asyncpg.Connection,
    *,
    start_ts: datetime,
    end_ts: datetime | None,
    symbols: list[str],
) -> list[asyncpg.Record]:
    return await conn.fetch(
        """
        WITH event_tokens AS (
            SELECT
                market_slug,
                trim(both '"' from ((raw_market->'markets'->0->>'clobTokenIds')::jsonb->>0)) AS up_token,
                trim(both '"' from ((raw_market->'markets'->0->>'clobTokenIds')::jsonb->>1)) AS down_token
            FROM pm_market_metadata
            WHERE end_time >= $1::timestamptz
              AND ($2::timestamptz IS NULL OR end_time < $2::timestamptz)
              AND symbol = ANY($3::text[])
        )
        SELECT DISTINCT et.market_slug, et.up_token, et.down_token
        FROM event_tokens et
        WHERE et.market_slug IS NOT NULL
          AND et.up_token IS NOT NULL
          AND et.down_token IS NOT NULL
          AND et.up_token <> ''
          AND et.down_token <> ''
        ORDER BY et.market_slug
        """,
        start_ts,
        end_ts,
        symbols,
    )


def settlement_rows_from_gamma(
    data: dict[str, Any],
    *,
    up_token: str,
    down_token: str,
) -> list[tuple[str, str, float]]:
    if data.get("closed") is not True:
        return []
    try:
        token_ids = json.loads(data.get("clobTokenIds") or "[]")
        outcome_prices = json.loads(data.get("outcomePrices") or "[]")
    except json.JSONDecodeError:
        return []
    if len(token_ids) != 2 or len(outcome_prices) != 2:
        return []

    settlements = []
    for token_id, raw_price in zip(token_ids, outcome_prices):
        price = float(raw_price)
        if price >= 0.95:
            settlements.append((str(token_id), "winner", 1.0))
        elif price <= 0.05:
            settlements.append((str(token_id), "loser", 0.0))
        else:
            return []

    matched = [item for item in settlements if item[0] in {up_token, down_token}]
    return matched if len(matched) == 2 else []


async def upsert_settlement(
    conn: asyncpg.Connection,
    *,
    market_slug: str,
    token_id: str,
    outcome: str,
    settled_price: float,
) -> bool:
    result = await conn.execute(
        """
        INSERT INTO pm_token_settlements (
            token_id, market_slug, outcome,
            settled_price, resolved, resolved_at, fetched_at
        ) VALUES ($1, $2, $3, $4, true, NOW(), NOW())
        ON CONFLICT (token_id) DO UPDATE SET
            settled_price = EXCLUDED.settled_price,
            outcome = EXCLUDED.outcome,
            resolved = true,
            resolved_at = COALESCE(pm_token_settlements.resolved_at, NOW()),
            fetched_at = NOW()
        WHERE pm_token_settlements.resolved = false
           OR pm_token_settlements.settled_price IS DISTINCT FROM EXCLUDED.settled_price
           OR pm_token_settlements.outcome IS DISTINCT FROM EXCLUDED.outcome
        """,
        token_id,
        market_slug,
        outcome,
        settled_price,
    )
    return result != "INSERT 0 0"


async def run(args: argparse.Namespace) -> dict[str, Any]:
    db_url = args.db_url or args.legacy_db_url
    if not db_url:
        db_url = "postgresql://postgres:postgres@localhost:15432/ploy"
    symbols = parse_symbols(args.symbols)
    if not symbols:
        raise SystemExit("--symbols must include at least one symbol")
    start_ts = parse_utc_ts(args.start_ts)
    if start_ts is None:
        raise SystemExit("--start-ts is required")
    end_ts = parse_utc_ts(args.end_ts)

    conn = await asyncpg.connect(db_url)
    try:
        rows = await load_candidate_markets(
            conn,
            start_ts=start_ts,
            end_ts=end_ts,
            symbols=symbols,
        )

        settled = 0
        would_settle = 0
        errors = 0
        skipped = 0
        active_reset = 0

        async with httpx.AsyncClient(timeout=5.0) as client:
            for i, row in enumerate(rows):
                market_slug = row["market_slug"]
                up_token = row["up_token"]
                down_token = row["down_token"]

                try:
                    resp = await client.get(
                        f"https://gamma-api.polymarket.com/markets/{market_slug}",
                        headers={"User-Agent": "ploy-settlement-backfill/official"},
                    )
                    resp.raise_for_status()
                    data = resp.json()

                    if data.get("closed") is False:
                        if not args.dry_run:
                            await conn.execute(
                                """
                                UPDATE pm_token_settlements
                                SET settled_price = NULL,
                                    outcome = NULL,
                                    resolved = FALSE,
                                    resolved_at = NULL,
                                    fetched_at = NOW()
                                WHERE market_slug = $1
                                  AND resolved = TRUE
                                """,
                                market_slug,
                            )
                        active_reset += 1
                        skipped += 1
                        continue

                    settlements = settlement_rows_from_gamma(
                        data,
                        up_token=up_token,
                        down_token=down_token,
                    )
                    if not settlements:
                        skipped += 1
                        continue

                    for token_id, outcome, settled_price in settlements:
                        if args.dry_run:
                            would_settle += 1
                        elif await upsert_settlement(
                            conn,
                            market_slug=market_slug,
                            token_id=token_id,
                            outcome=outcome,
                            settled_price=settled_price,
                        ):
                            settled += 1

                except Exception:
                    errors += 1

                if (i + 1) % 100 == 0:
                    print(
                        "Progress: "
                        f"{i + 1}/{len(rows)} settled={settled} "
                        f"would_settle={would_settle} skipped={skipped} errors={errors}"
                    )

                await asyncio.sleep(0.2)

        return {
            "schema_version": "official_settlement_repair.v1",
            "dry_run": args.dry_run,
            "start_ts": args.start_ts,
            "end_ts": args.end_ts,
            "symbols": symbols,
            "candidate_market_count": len(rows),
            "settled_count": settled,
            "would_settle_count": would_settle,
            "active_reset_count": active_reset,
            "skipped_count": skipped,
            "error_count": errors,
        }
    finally:
        await conn.close()


async def main_async() -> None:
    args = parser().parse_args()
    report = await run(args)
    text = json.dumps(report, indent=2, sort_keys=True) + "\n"
    if args.report_json:
        args.report_json.write_text(text, encoding="utf-8")
    print(text)


def main() -> None:
    asyncio.run(main_async())


if __name__ == "__main__":
    main()
