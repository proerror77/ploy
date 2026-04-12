#!/usr/bin/env python3
"""
Backfill settlement data for expired events using the official Gamma market API.
Falls back to skipping unresolved markets instead of inferring settlement from
last trade price.
"""
import asyncio
import asyncpg
import httpx
import json
import sys

DB_URL = sys.argv[1] if len(sys.argv) > 1 else "postgresql://postgres:postgres@localhost:15432/ploy"

async def main():
    conn = await asyncpg.connect(DB_URL)

    # Get all unsettled binary markets from expired events
    rows = await conn.fetch("""
        WITH event_tokens AS (
            SELECT
                market_slug,
                trim(both '"' from ((raw_market->'markets'->0->>'clobTokenIds')::jsonb->>0)) as up_token,
                trim(both '"' from ((raw_market->'markets'->0->>'clobTokenIds')::jsonb->>1)) as down_token
            FROM pm_market_metadata
            WHERE end_time >= '2026-04-05' AND end_time < NOW()
              AND symbol IN ('BTCUSDT','ETHUSDT','SOLUSDT','XRPUSDT','DOGEUSDT','HYPEUSDT','BNBUSDT')
        )
        SELECT DISTINCT et.market_slug, et.up_token, et.down_token
        FROM event_tokens et
        ORDER BY et.market_slug
    """)

    print(f"Found {len(rows)} unsettled markets")

    settled = 0
    errors = 0
    skipped = 0

    async with httpx.AsyncClient(timeout=5.0) as client:
        for i, row in enumerate(rows):
            market_id = row['market_slug']
            up_token = row['up_token']
            down_token = row['down_token']

            try:
                resp = await client.get(
                    f"https://gamma-api.polymarket.com/markets/{market_id}",
                    headers={"User-Agent": "ploy-settlement-backfill/official"},
                )
                resp.raise_for_status()
                data = resp.json()

                if not data.get('closed'):
                    await conn.execute("""
                        UPDATE pm_token_settlements
                        SET settled_price = NULL,
                            outcome = NULL,
                            resolved = FALSE,
                            resolved_at = NULL,
                            fetched_at = NOW()
                        WHERE market_slug = $1
                          AND resolved = TRUE
                    """, market_id)
                    skipped += 1
                    continue

                token_ids = json.loads(data.get('clobTokenIds') or '[]')
                outcome_prices = json.loads(data.get('outcomePrices') or '[]')
                if len(token_ids) != 2 or len(outcome_prices) != 2:
                    skipped += 1
                    continue

                settlements = []
                for token_id, raw_price in zip(token_ids, outcome_prices):
                    price = float(raw_price)
                    if price >= 0.95:
                        settlements.append((token_id, 'winner', 1.0))
                    elif price <= 0.05:
                        settlements.append((token_id, 'loser', 0.0))
                    else:
                        settlements = []
                        break

                if len(settlements) != 2:
                    skipped += 1
                    continue

                for token_id, outcome, settled_price in settlements:
                    if token_id not in (up_token, down_token):
                        continue

                    result = await conn.execute("""
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
                    """, token_id, market_id, outcome, settled_price)

                    if result != 'INSERT 0 0':
                        settled += 1

            except Exception as e:
                errors += 1

            if (i + 1) % 100 == 0:
                print(f"  Progress: {i+1}/{len(rows)} | settled={settled} skipped={skipped} errors={errors}")

            # Rate limit: 5 req/sec
            await asyncio.sleep(0.2)

    print(f"\nDone: {settled} settled, {skipped} skipped (active/no data), {errors} errors")
    await conn.close()

asyncio.run(main())
