-- Migration: 023_clob_quote_ticks_dedup
-- Description: Add domain column, relax side to nullable (live feed has no side info),
--              and add a unique constraint so ON CONFLICT DO NOTHING actually deduplicates
--              REST-polled quotes that arrive within the same second for the same token.

-- Add domain column if missing (collector.rs inserts it but migration 010 omitted it)
ALTER TABLE clob_quote_ticks
    ADD COLUMN IF NOT EXISTS domain TEXT;

-- Relax side to nullable so live-feed inserts don't require it
ALTER TABLE clob_quote_ticks
    ALTER COLUMN side DROP NOT NULL;

-- Unique constraint: one quote per token per second.
-- date_trunc('second', received_at) collapses the 5-second REST poll cadence so
-- repeated polls within the same second are silently dropped.
CREATE UNIQUE INDEX IF NOT EXISTS uq_clob_quote_ticks_token_second
    ON clob_quote_ticks (token_id, date_trunc('second', received_at));

-- Unique constraint on binance_price_ticks: one tick per symbol per second.
-- spawn_spot_feed throttles writes to 1/sec in-process; this index is the DB-side guard.
CREATE UNIQUE INDEX IF NOT EXISTS uq_binance_price_ticks_symbol_second
    ON binance_price_ticks (symbol, date_trunc('second', trade_time));
