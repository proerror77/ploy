#!/usr/bin/env python3
"""
Backfill settlement data for today's expired events.
Uses /last-trade-price API to determine winner/loser.
"""
import asyncio
import asyncpg
import httpx
import sys

DB_URL = sys.argv[1] if len(sys.argv) > 1 else "postgresql://postgres:postgres@localhost:15432/ploy"

async def main():
    conn = await asyncpg.connect(DB_URL)

    # Get all unsettled tokens from today's expired events
    rows = await conn.fetch("""
        WITH event_tokens AS (
            SELECT
                trim(both '"' from jsonb_array_elements_text(
                    (raw_market->'markets'->0->>'clobTokenIds')::jsonb
                )) as token_id,
                market_slug
            FROM pm_market_metadata
            WHERE end_time >= '2026-04-05' AND end_time < NOW()
              AND symbol IN ('BTCUSDT','ETHUSDT','SOLUSDT','XRPUSDT','DOGEUSDT','HYPEUSDT','BNBUSDT')
        )
        SELECT DISTINCT et.token_id, et.market_slug
        FROM event_tokens et
        WHERE NOT EXISTS (
            SELECT 1 FROM pm_token_settlements s
            WHERE s.token_id = et.token_id AND s.resolved = TRUE
        )
        ORDER BY et.market_slug
    """)

    print(f"Found {len(rows)} unsettled tokens")

    settled = 0
    errors = 0
    skipped = 0

    async with httpx.AsyncClient(timeout=5.0) as client:
        for i, row in enumerate(rows):
            token_id = row['token_id']
            slug = row['market_slug']

            try:
                resp = await client.get(
                    f"https://clob.polymarket.com/last-trade-price?token_id={token_id}"
                )
                data = resp.json()
                price_str = data.get('price')
                if not price_str:
                    skipped += 1
                    continue

                price = float(price_str)
                is_winner = price >= 0.95
                is_loser = price <= 0.05

                if not is_winner and not is_loser:
                    skipped += 1
                    continue

                settled_price = 1.0 if is_winner else 0.0
                outcome = 'winner' if is_winner else 'loser'

                result = await conn.execute("""
                    INSERT INTO pm_token_settlements (
                        token_id, market_slug, outcome,
                        settled_price, resolved, resolved_at, fetched_at
                    ) VALUES ($1, $2, $3, $4, true, NOW(), NOW())
                    ON CONFLICT (token_id) DO UPDATE SET
                        settled_price = EXCLUDED.settled_price,
                        resolved = true,
                        resolved_at = COALESCE(pm_token_settlements.resolved_at, NOW()),
                        fetched_at = NOW()
                    WHERE pm_token_settlements.resolved = false
                """, token_id, slug, outcome, settled_price)

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
