#!/usr/bin/env python3
"""
Backfill clob_quote_ticks for missing dates using synthetic data.

Strategy:
1. Analyze existing quote data (03-24, 03-25, 03-28) to extract patterns
2. For each missing date, generate quotes based on:
   - Event windows from pm_market_metadata
   - Spot price movements from binance_price_ticks
   - Typical bid-ask spread from existing quotes
3. Insert synthetic quotes with source='backfill_synthetic'
"""

import psycopg2
from datetime import datetime, timedelta
from decimal import Decimal
import sys

DB_URL = "postgresql://postgres:postgres@localhost:5432/ploy"

# Missing date ranges
MISSING_DATES = [
    ("2026-03-12", "2026-03-23"),  # 12 days
    ("2026-03-26", "2026-03-27"),  # 2 days
    ("2026-03-29", "2026-03-31"),  # 3 days
]

def analyze_existing_quotes(conn):
    """Analyze existing quote data to extract patterns."""
    cursor = conn.cursor()

    # Get average spread and quote frequency
    cursor.execute("""
        SELECT
            AVG(best_ask - best_bid) as avg_spread,
            STDDEV(best_ask - best_bid) as stddev_spread,
            COUNT(*) as total_quotes,
            COUNT(DISTINCT token_id) as unique_tokens,
            MIN(received_at) as first_quote,
            MAX(received_at) as last_quote
        FROM clob_quote_ticks
        WHERE received_at >= '2026-03-24'
          AND received_at <= '2026-03-28'
          AND best_bid IS NOT NULL
          AND best_ask IS NOT NULL
    """)

    stats = cursor.fetchone()
    cursor.close()

    return {
        'avg_spread': float(stats[0]) if stats[0] else 0.02,
        'stddev_spread': float(stats[1]) if stats[1] else 0.01,
        'total_quotes': stats[2],
        'unique_tokens': stats[3],
        'first_quote': stats[4],
        'last_quote': stats[5],
    }

def get_events_for_date_range(conn, start_date, end_date):
    """Get all events in the date range."""
    cursor = conn.cursor()

    cursor.execute("""
        SELECT
            market_slug,
            symbol,
            start_time,
            end_time,
            ((raw_market->'markets'->0->>'clobTokenIds')::jsonb->0)::text AS up_token_id,
            ((raw_market->'markets'->0->>'clobTokenIds')::jsonb->1)::text AS down_token_id,
            price_to_beat
        FROM pm_market_metadata
        WHERE symbol IN ('BTCUSDT', 'ETHUSDT', 'SOLUSDT')
          AND end_time >= %s
          AND start_time <= %s
          AND raw_market->'markets'->0->'clobTokenIds' IS NOT NULL
        ORDER BY start_time
    """, (start_date, end_date))

    events = cursor.fetchall()
    cursor.close()

    return events

def get_spot_price_at_time(conn, symbol, timestamp):
    """Get spot price closest to the given timestamp."""
    cursor = conn.cursor()

    cursor.execute("""
        SELECT price
        FROM binance_price_ticks
        WHERE symbol = %s
          AND trade_time <= %s
        ORDER BY trade_time DESC
        LIMIT 1
    """, (symbol, timestamp))

    result = cursor.fetchone()
    cursor.close()

    return float(result[0]) if result else None

def estimate_probability(spot_price, price_to_beat, direction='UP'):
    """
    Estimate probability using log-normal model.
    This is a simplified version of the DirectionalSignalEvaluator logic.
    """
    if spot_price is None or price_to_beat is None:
        return 0.5

    # Simple linear approximation for now
    # In reality, this should use the full log-normal model
    if direction == 'UP':
        if spot_price > price_to_beat:
            return min(0.95, 0.5 + (spot_price - price_to_beat) / price_to_beat)
        else:
            return max(0.05, 0.5 - (price_to_beat - spot_price) / price_to_beat)
    else:  # DOWN
        return 1.0 - estimate_probability(spot_price, price_to_beat, 'UP')

def generate_quotes_for_event(conn, event, stats):
    """Generate synthetic quotes for an event."""
    market_slug, symbol, start_time, end_time, up_token, down_token, price_to_beat = event

    # Strip quotes from token IDs (they come as "\"123\"" from JSONB)
    up_token = up_token.strip('"')
    down_token = down_token.strip('"')

    quotes = []

    # Generate quotes every 30 seconds during the event window
    current_time = start_time
    interval = timedelta(seconds=30)

    while current_time <= end_time:
        # Get spot price at this time
        spot_price = get_spot_price_at_time(conn, symbol, current_time)

        if spot_price and price_to_beat:
            # Estimate probabilities
            p_up = estimate_probability(spot_price, float(price_to_beat), 'UP')
            p_down = 1.0 - p_up

            # Generate bid/ask with typical spread
            spread = stats['avg_spread']

            # UP token quotes
            up_mid = Decimal(str(p_up))
            up_bid = max(Decimal('0.01'), up_mid - Decimal(str(spread/2)))
            up_ask = min(Decimal('0.99'), up_mid + Decimal(str(spread/2)))

            quotes.append({
                'token_id': up_token,
                'side': 'UP',
                'best_bid': up_bid,
                'best_ask': up_ask,
                'received_at': current_time,
            })

            # DOWN token quotes
            down_mid = Decimal(str(p_down))
            down_bid = max(Decimal('0.01'), down_mid - Decimal(str(spread/2)))
            down_ask = min(Decimal('0.99'), down_mid + Decimal(str(spread/2)))

            quotes.append({
                'token_id': down_token,
                'side': 'DOWN',
                'best_bid': down_bid,
                'best_ask': down_ask,
                'received_at': current_time,
            })

        current_time += interval

    return quotes

def insert_quotes(conn, quotes):
    """Insert synthetic quotes into database."""
    cursor = conn.cursor()

    for quote in quotes:
        cursor.execute("""
            INSERT INTO clob_quote_ticks
                (token_id, side, best_bid, best_ask, received_at, source, domain)
            VALUES (%s, %s, %s, %s, %s, 'backfill_synthetic', 'crypto')
            ON CONFLICT DO NOTHING
        """, (
            quote['token_id'],
            quote['side'],
            quote['best_bid'],
            quote['best_ask'],
            quote['received_at'],
        ))

    conn.commit()
    cursor.close()

def main():
    print("Connecting to database...")
    conn = psycopg2.connect(DB_URL)

    try:
        print("Analyzing existing quote data...")
        stats = analyze_existing_quotes(conn)
        print(f"  Average spread: {stats['avg_spread']:.4f}")
        print(f"  Total quotes: {stats['total_quotes']}")
        print(f"  Unique tokens: {stats['unique_tokens']}")

        total_inserted = 0

        for start_date, end_date in MISSING_DATES:
            print(f"\nProcessing date range: {start_date} to {end_date}")

            # Get events for this date range
            events = get_events_for_date_range(conn, start_date, end_date)
            print(f"  Found {len(events)} events")

            for i, event in enumerate(events):
                if i % 100 == 0:
                    print(f"  Processing event {i+1}/{len(events)}...")

                # Generate quotes for this event
                quotes = generate_quotes_for_event(conn, event, stats)

                # Insert quotes
                if quotes:
                    insert_quotes(conn, quotes)
                    total_inserted += len(quotes)

        print(f"\n✅ Backfill complete! Inserted {total_inserted} synthetic quotes")

    except Exception as e:
        print(f"❌ Error: {e}")
        conn.rollback()
        sys.exit(1)
    finally:
        conn.close()

if __name__ == "__main__":
    main()
