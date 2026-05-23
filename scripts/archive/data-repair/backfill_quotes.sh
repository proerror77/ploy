#!/bin/bash
# Backfill clob_quote_ticks using SQL-based approach
# This script generates synthetic quotes based on existing patterns

set -euo pipefail

DB_URL="postgresql://postgres:postgres@localhost:5432/ploy"

echo "=== Quote Data Backfill Script ==="
echo "Analyzing existing quote data..."

# Get average spread from existing quotes
AVG_SPREAD=$(psql "$DB_URL" -t -c "
    SELECT COALESCE(AVG(best_ask - best_bid), 0.02)
    FROM clob_quote_ticks
    WHERE received_at >= '2026-03-24'
      AND received_at <= '2026-03-28'
      AND best_bid IS NOT NULL
      AND best_ask IS NOT NULL
")

echo "Average spread: $AVG_SPREAD"

# Function to backfill quotes for a date range
backfill_date_range() {
    local start_date=$1
    local end_date=$2

    echo ""
    echo "Processing date range: $start_date to $end_date"

    # Generate quotes using SQL
    psql "$DB_URL" <<EOF
    -- Create temporary table for synthetic quotes
    CREATE TEMP TABLE IF NOT EXISTS temp_synthetic_quotes (
        token_id text,
        side text,
        best_bid numeric(10,6),
        best_ask numeric(10,6),
        received_at timestamp with time zone
    );

    -- Generate synthetic quotes for each event
    -- Strategy: Use spot price to estimate probability, then generate bid/ask
    WITH events AS (
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
          AND end_time >= '$start_date'::timestamp
          AND start_time <= '$end_date'::timestamp + interval '1 day'
          AND raw_market->'markets'->0->'clobTokenIds' IS NOT NULL
    ),
    time_series AS (
        -- Generate timestamps every 30 seconds during event windows
        SELECT
            e.*,
            generate_series(
                e.start_time,
                e.end_time,
                interval '30 seconds'
            ) AS quote_time
        FROM events e
    ),
    with_spot_prices AS (
        -- Join with nearest spot price
        SELECT
            ts.*,
            (
                SELECT price
                FROM binance_price_ticks bpt
                WHERE bpt.symbol = ts.symbol
                  AND bpt.trade_time <= ts.quote_time
                ORDER BY bpt.trade_time DESC
                LIMIT 1
            ) AS spot_price
        FROM time_series ts
    ),
    with_probabilities AS (
        -- Estimate probabilities (simplified linear model)
        SELECT
            *,
            CASE
                WHEN spot_price IS NULL OR price_to_beat IS NULL THEN 0.5
                WHEN spot_price > price_to_beat THEN
                    LEAST(0.95, 0.5 + (spot_price - price_to_beat) / NULLIF(price_to_beat, 0))
                ELSE
                    GREATEST(0.05, 0.5 - (price_to_beat - spot_price) / NULLIF(price_to_beat, 0))
            END AS p_up
        FROM with_spot_prices
        WHERE spot_price IS NOT NULL
    )
    -- Insert UP token quotes
    INSERT INTO clob_quote_ticks (token_id, side, best_bid, best_ask, received_at, source, domain)
    SELECT
        TRIM(BOTH '"' FROM up_token_id) AS token_id,
        'UP' AS side,
        GREATEST(0.01, p_up - $AVG_SPREAD / 2.0)::numeric(10,6) AS best_bid,
        LEAST(0.99, p_up + $AVG_SPREAD / 2.0)::numeric(10,6) AS best_ask,
        quote_time AS received_at,
        'backfill_synthetic' AS source,
        'crypto' AS domain
    FROM with_probabilities
    ON CONFLICT DO NOTHING;

    -- Insert DOWN token quotes
    INSERT INTO clob_quote_ticks (token_id, side, best_bid, best_ask, received_at, source, domain)
    SELECT
        TRIM(BOTH '"' FROM down_token_id) AS token_id,
        'DOWN' AS side,
        GREATEST(0.01, (1.0 - p_up) - $AVG_SPREAD / 2.0)::numeric(10,6) AS best_bid,
        LEAST(0.99, (1.0 - p_up) + $AVG_SPREAD / 2.0)::numeric(10,6) AS best_ask,
        quote_time AS received_at,
        'backfill_synthetic' AS source,
        'crypto' AS domain
    FROM with_probabilities
    ON CONFLICT DO NOTHING;

    -- Report inserted count
    SELECT COUNT(*) AS inserted_quotes
    FROM clob_quote_ticks
    WHERE source = 'backfill_synthetic'
      AND received_at >= '$start_date'::timestamp
      AND received_at <= '$end_date'::timestamp + interval '1 day';
EOF
}

# Backfill missing date ranges
backfill_date_range "2026-03-12" "2026-03-23"
backfill_date_range "2026-03-26" "2026-03-27"
backfill_date_range "2026-03-29" "2026-03-31"

echo ""
echo "=== Backfill Summary ==="
psql "$DB_URL" -c "
    SELECT
        DATE(received_at) AS date,
        COUNT(*) AS quotes,
        source
    FROM clob_quote_ticks
    WHERE received_at >= '2026-03-12'
      AND received_at <= '2026-03-31'
    GROUP BY DATE(received_at), source
    ORDER BY date, source;
"

echo ""
echo "✅ Backfill complete!"
