#!/bin/bash
# Final improved quote backfill - handles missing early spot prices
# Uses nearest available spot price (forward or backward lookup)

set -euo pipefail

DB_URL="postgresql://postgres:postgres@localhost:5432/ploy"

echo "=== Final Improved Quote Data Backfill Script ==="

# Ensure normal_cdf function exists
psql "$DB_URL" -c "SELECT normal_cdf(0.0);" > /dev/null 2>&1 || {
    echo "Creating normal_cdf function..."
    psql "$DB_URL" <<'EOF'
CREATE OR REPLACE FUNCTION normal_cdf(x double precision)
RETURNS double precision AS $$
DECLARE
    t double precision;
    z double precision;
    result double precision;
BEGIN
    IF x > 6 THEN RETURN 1.0; END IF;
    IF x < -6 THEN RETURN 0.0; END IF;
    z := abs(x);
    t := 1.0 / (1.0 + 0.2316419 * z);
    result := 1.0 - (1.0 / sqrt(2.0 * pi())) * exp(-z * z / 2.0) *
              (0.319381530 * t + -0.356563782 * t * t + 1.781477937 * t * t * t +
               -1.821255978 * t * t * t * t + 1.330274429 * t * t * t * t * t);
    RETURN CASE WHEN x >= 0 THEN result ELSE 1.0 - result END;
END;
$$ LANGUAGE plpgsql IMMUTABLE;
EOF
}

echo "Analyzing existing quote data..."
AVG_SPREAD=$(psql "$DB_URL" -t -c "
    SELECT COALESCE(AVG(best_ask - best_bid), 0.016)
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

    psql "$DB_URL" <<EOF
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
        -- Use nearest spot price (prefer backward, fallback to forward)
        SELECT
            ts.*,
            COALESCE(
                (SELECT price FROM binance_price_ticks bpt
                 WHERE bpt.symbol = ts.symbol AND bpt.trade_time <= ts.quote_time
                 ORDER BY bpt.trade_time DESC LIMIT 1),
                (SELECT price FROM binance_price_ticks bpt
                 WHERE bpt.symbol = ts.symbol AND bpt.trade_time >= ts.quote_time
                 ORDER BY bpt.trade_time ASC LIMIT 1)
            ) AS spot_price
        FROM time_series ts
    ),
    with_probabilities AS (
        SELECT
            *,
            EXTRACT(EPOCH FROM (end_time - quote_time)) AS secs_remaining,
            CASE
                WHEN spot_price IS NULL OR price_to_beat IS NULL THEN 0.5
                WHEN spot_price <= 0 OR price_to_beat <= 0 THEN 0.5
                ELSE
                    normal_cdf(
                        ln(spot_price / price_to_beat) /
                        (0.001 * sqrt(GREATEST(EXTRACT(EPOCH FROM (end_time - quote_time)), 1.0) / 900.0))
                    )
            END AS p_up
        FROM with_spot_prices
        WHERE spot_price IS NOT NULL
          AND EXTRACT(EPOCH FROM (end_time - quote_time)) > 0
    ),
    all_quotes AS (
        SELECT
            TRIM(BOTH '"' FROM up_token_id) AS token_id,
            'UP' AS side,
            GREATEST(0.01, LEAST(0.99, p_up - $AVG_SPREAD / 2.0))::numeric(10,6) AS best_bid,
            GREATEST(0.01, LEAST(0.99, p_up + $AVG_SPREAD / 2.0))::numeric(10,6) AS best_ask,
            quote_time AS received_at,
            'backfill_final' AS source,
            'crypto' AS domain
        FROM with_probabilities
        WHERE p_up IS NOT NULL

        UNION ALL

        SELECT
            TRIM(BOTH '"' FROM down_token_id) AS token_id,
            'DOWN' AS side,
            GREATEST(0.01, LEAST(0.99, (1.0 - p_up) - $AVG_SPREAD / 2.0))::numeric(10,6) AS best_bid,
            GREATEST(0.01, LEAST(0.99, (1.0 - p_up) + $AVG_SPREAD / 2.0))::numeric(10,6) AS best_ask,
            quote_time AS received_at,
            'backfill_final' AS source,
            'crypto' AS domain
        FROM with_probabilities
        WHERE p_up IS NOT NULL
    )
    INSERT INTO clob_quote_ticks (token_id, side, best_bid, best_ask, received_at, source, domain)
    SELECT * FROM all_quotes
    ON CONFLICT DO NOTHING;

    SELECT
        COUNT(*) AS total_quotes,
        COUNT(DISTINCT token_id) AS unique_tokens,
        COUNT(CASE WHEN side = 'UP' THEN 1 END) AS up_quotes,
        COUNT(CASE WHEN side = 'DOWN' THEN 1 END) AS down_quotes,
        ROUND(MIN(best_ask)::numeric, 4) AS min_ask,
        ROUND(MAX(best_ask)::numeric, 4) AS max_ask,
        ROUND(AVG(best_ask)::numeric, 4) AS avg_ask,
        ROUND(STDDEV(best_ask)::numeric, 4) AS stddev_ask
    FROM clob_quote_ticks
    WHERE source = 'backfill_final'
      AND received_at >= '$start_date'::timestamp
      AND received_at <= '$end_date'::timestamp + interval '1 day';
EOF
}

echo ""
echo "Clearing old synthetic data..."
psql "$DB_URL" -c "DELETE FROM clob_quote_ticks WHERE source LIKE 'backfill%';"

backfill_date_range "2026-03-12" "2026-03-23"
backfill_date_range "2026-03-26" "2026-03-27"
backfill_date_range "2026-03-29" "2026-03-31"

echo ""
echo "=== Final Summary ==="
psql "$DB_URL" -c "
    SELECT
        source,
        COUNT(*) AS total_quotes,
        ROUND(MIN(best_ask)::numeric, 4) AS min_ask,
        ROUND(MAX(best_ask)::numeric, 4) AS max_ask,
        ROUND(AVG(best_ask)::numeric, 4) AS avg_ask,
        ROUND(STDDEV(best_ask)::numeric, 4) AS stddev_ask
    FROM clob_quote_ticks
    WHERE received_at >= '2026-03-12'
      AND received_at <= '2026-03-31'
    GROUP BY source
    ORDER BY source;
"

echo ""
echo "=== Sample UP/DOWN Pairs ==="
psql "$DB_URL" -c "
    SELECT
        ROUND(q1.best_ask::numeric, 4) AS up_ask,
        ROUND(q2.best_ask::numeric, 4) AS down_ask,
        ROUND((q1.best_ask + q2.best_ask)::numeric, 4) AS sum,
        q1.received_at
    FROM clob_quote_ticks q1
    JOIN clob_quote_ticks q2
        ON q1.received_at = q2.received_at
        AND q1.token_id != q2.token_id
    WHERE q1.source = 'backfill_final'
      AND q2.source = 'backfill_final'
      AND q1.side = 'UP'
      AND q2.side = 'DOWN'
      AND q1.received_at >= '2026-03-12 15:45:00'
      AND q1.received_at <= '2026-03-12 15:50:00'
    LIMIT 10;
"

echo ""
echo "✅ Final backfill complete!"
