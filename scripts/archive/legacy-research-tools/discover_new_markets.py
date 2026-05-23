#!/usr/bin/env python3
"""
Discover and insert DOGE/HYPE/BNB 5-minute markets into database.
"""
import asyncio
import asyncpg
import httpx
from datetime import datetime, timezone

GAMMA_API = "https://gamma-api.polymarket.com"
DB_URL = "postgresql://postgres:postgres@localhost:5432/ploy"

SYMBOLS = {
    "DOGE": "DOGEUSDT",
    "HYPE": "HYPEUSDT",
    "BNB": "BNBUSDT"
}

async def fetch_markets(symbol_keyword: str):
    """Fetch markets from Polymarket Gamma API."""
    async with httpx.AsyncClient(timeout=30.0) as client:
        response = await client.get(
            f"{GAMMA_API}/markets",
            params={
                "closed": "false",
                "limit": 100,
            }
        )
        response.raise_for_status()
        markets = response.json()

        # Filter for 5-minute markets matching symbol
        filtered = []
        for market in markets:
            question = market.get("question", "").upper()
            slug = market.get("market_slug", "")

            if symbol_keyword.upper() in question and "-5M" in slug.upper():
                filtered.append(market)

        return filtered

async def insert_market(conn, market, symbol):
    """Insert market into pm_market_metadata."""
    market_slug = market["market_slug"]

    # Check if already exists
    exists = await conn.fetchval(
        "SELECT 1 FROM pm_market_metadata WHERE market_slug = $1",
        market_slug
    )

    if exists:
        print(f"  ⏭️  {market_slug} already exists")
        return False

    # Extract data
    start_time = datetime.fromisoformat(market["start_date_iso"].replace("Z", "+00:00"))
    end_time = datetime.fromisoformat(market["end_date_iso"].replace("Z", "+00:00"))
    condition_id = market["condition_id"]

    # Insert
    await conn.execute("""
        INSERT INTO pm_market_metadata (
            market_slug,
            symbol,
            start_time,
            end_time,
            condition_id,
            raw_market,
            discovered_at
        ) VALUES ($1, $2, $3, $4, $5, $6, NOW())
    """, market_slug, symbol, start_time, end_time, condition_id, market)

    print(f"  ✅ Inserted {market_slug}")
    return True

async def main():
    print("🔍 Discovering new markets for DOGE, HYPE, BNB...")

    conn = await asyncpg.connect(DB_URL)

    try:
        total_inserted = 0

        for keyword, symbol in SYMBOLS.items():
            print(f"\n📊 Fetching {keyword} markets...")
            markets = await fetch_markets(keyword)
            print(f"   Found {len(markets)} markets")

            for market in markets:
                inserted = await insert_market(conn, market, symbol)
                if inserted:
                    total_inserted += 1

        print(f"\n✨ Done! Inserted {total_inserted} new markets")

    finally:
        await conn.close()

if __name__ == "__main__":
    asyncio.run(main())
