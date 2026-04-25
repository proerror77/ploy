#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 1 ]]; then
  echo "usage: $0 YYYY-MM-DD" >&2
  exit 64
fi

repair_date="$1"
if [[ ! "${repair_date}" =~ ^[0-9]{4}-[0-9]{2}-[0-9]{2}$ ]]; then
  echo "repair date must be YYYY-MM-DD: ${repair_date}" >&2
  exit 64
fi

db_url="${DB_URL:-postgresql://postgres:postgres@localhost:5432/ploy}"
start_ts="${repair_date}T00:00:00Z"
end_ts="$(date -u -d "${repair_date} + 1 day" +%Y-%m-%dT00:00:00Z)"

psql "${db_url}" \
  --set=ON_ERROR_STOP=1 \
  --set=start_ts="${start_ts}" \
  --set=end_ts="${end_ts}" <<'SQL'
\echo 'Repairing clob_quote_ticks bid_size/ask_size from clob_orderbook_snapshots'
\echo 'Window:' :start_ts 'to' :end_ts

-- The quote table is a compressed TimescaleDB hypertable on Tango. A one-day
-- repair can legitimately touch more than the default 100k decompressed tuple
-- DML guard. Keep the override scoped to this psql session.
SET timescaledb.max_tuples_decompressed_per_dml_transaction = 0;

WITH before_counts AS (
    SELECT
        count(*) AS quote_rows,
        count(*) FILTER (WHERE ask_size IS NOT NULL AND ask_size > 0) AS ask_size_rows,
        count(*) FILTER (WHERE bid_size IS NOT NULL AND bid_size > 0) AS bid_size_rows
    FROM clob_quote_ticks
    WHERE received_at >= :'start_ts'::timestamptz
      AND received_at < :'end_ts'::timestamptz
      AND source = 'polymarket_ws_collector'
),
candidates AS (
    SELECT
        q.id,
        (
            SELECT (level->>'size')::numeric
            FROM clob_orderbook_snapshots s
            CROSS JOIN LATERAL jsonb_array_elements(s.bids) AS level
            WHERE s.token_id = q.token_id
              AND s.received_at = q.received_at
              AND q.best_bid IS NOT NULL
              AND (level->>'price')::numeric = q.best_bid
              AND (level->>'size')::numeric > 0
            ORDER BY (level->>'size')::numeric DESC
            LIMIT 1
        ) AS repaired_bid_size,
        (
            SELECT (level->>'size')::numeric
            FROM clob_orderbook_snapshots s
            CROSS JOIN LATERAL jsonb_array_elements(s.asks) AS level
            WHERE s.token_id = q.token_id
              AND s.received_at = q.received_at
              AND q.best_ask IS NOT NULL
              AND (level->>'price')::numeric = q.best_ask
              AND (level->>'size')::numeric > 0
            ORDER BY (level->>'size')::numeric DESC
            LIMIT 1
        ) AS repaired_ask_size
    FROM clob_quote_ticks q
    WHERE q.received_at >= :'start_ts'::timestamptz
      AND q.received_at < :'end_ts'::timestamptz
      AND q.source = 'polymarket_ws_collector'
      AND (q.bid_size IS NULL OR q.ask_size IS NULL)
),
updated AS (
    UPDATE clob_quote_ticks q
    SET
        bid_size = COALESCE(q.bid_size, c.repaired_bid_size),
        ask_size = COALESCE(q.ask_size, c.repaired_ask_size)
    FROM candidates c
    WHERE q.id = c.id
      AND (c.repaired_bid_size IS NOT NULL OR c.repaired_ask_size IS NOT NULL)
    RETURNING q.id
),
after_counts AS (
    SELECT
        count(*) AS quote_rows,
        count(*) FILTER (WHERE ask_size IS NOT NULL AND ask_size > 0) AS ask_size_rows,
        count(*) FILTER (WHERE bid_size IS NOT NULL AND bid_size > 0) AS bid_size_rows
    FROM clob_quote_ticks
    WHERE received_at >= :'start_ts'::timestamptz
      AND received_at < :'end_ts'::timestamptz
      AND source = 'polymarket_ws_collector'
)
SELECT
    (SELECT quote_rows FROM before_counts) AS before_quote_rows,
    (SELECT ask_size_rows FROM before_counts) AS before_ask_size_rows,
    (SELECT bid_size_rows FROM before_counts) AS before_bid_size_rows,
    (SELECT count(*) FROM updated) AS updated_rows,
    (SELECT ask_size_rows FROM after_counts) AS after_ask_size_rows,
    (SELECT bid_size_rows FROM after_counts) AS after_bid_size_rows;
SQL
