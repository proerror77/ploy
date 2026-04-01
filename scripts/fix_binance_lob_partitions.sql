-- Fix Binance LOB partitions for March 29 - April 7, 2026
-- Create missing daily partitions

-- March 29-31
CREATE TABLE IF NOT EXISTS binance_lob_ticks_20260329
PARTITION OF binance_lob_ticks
FOR VALUES FROM ('2026-03-29 00:00:00+08') TO ('2026-03-30 00:00:00+08');

CREATE TABLE IF NOT EXISTS binance_lob_ticks_20260330
PARTITION OF binance_lob_ticks
FOR VALUES FROM ('2026-03-30 00:00:00+08') TO ('2026-03-31 00:00:00+08');

CREATE TABLE IF NOT EXISTS binance_lob_ticks_20260331
PARTITION OF binance_lob_ticks
FOR VALUES FROM ('2026-03-31 00:00:00+08') TO ('2026-04-01 00:00:00+08');

-- April 2026
CREATE TABLE IF NOT EXISTS binance_lob_ticks_20260401
PARTITION OF binance_lob_ticks
FOR VALUES FROM ('2026-04-01 00:00:00+08') TO ('2026-04-02 00:00:00+08');

CREATE TABLE IF NOT EXISTS binance_lob_ticks_20260402
PARTITION OF binance_lob_ticks
FOR VALUES FROM ('2026-04-02 00:00:00+08') TO ('2026-04-03 00:00:00+08');

CREATE TABLE IF NOT EXISTS binance_lob_ticks_20260403
PARTITION OF binance_lob_ticks
FOR VALUES FROM ('2026-04-03 00:00:00+08') TO ('2026-04-04 00:00:00+08');

CREATE TABLE IF NOT EXISTS binance_lob_ticks_20260404
PARTITION OF binance_lob_ticks
FOR VALUES FROM ('2026-04-04 00:00:00+08') TO ('2026-04-05 00:00:00+08');

CREATE TABLE IF NOT EXISTS binance_lob_ticks_20260405
PARTITION OF binance_lob_ticks
FOR VALUES FROM ('2026-04-05 00:00:00+08') TO ('2026-04-06 00:00:00+08');

CREATE TABLE IF NOT EXISTS binance_lob_ticks_20260406
PARTITION OF binance_lob_ticks
FOR VALUES FROM ('2026-04-06 00:00:00+08') TO ('2026-04-07 00:00:00+08');

CREATE TABLE IF NOT EXISTS binance_lob_ticks_20260407
PARTITION OF binance_lob_ticks
FOR VALUES FROM ('2026-04-07 00:00:00+08') TO ('2026-04-08 00:00:00+08');

-- Verify partitions
SELECT
    schemaname,
    tablename,
    pg_size_pretty(pg_total_relation_size(schemaname||'.'||tablename)) AS size
FROM pg_tables
WHERE tablename LIKE 'binance_lob_ticks_2026%'
  AND tablename >= 'binance_lob_ticks_20260329'
ORDER BY tablename;

-- Show partition info
SELECT
    child.relname AS partition_name,
    pg_get_expr(child.relpartbound, child.oid) AS partition_expression
FROM pg_inherits
JOIN pg_class parent ON pg_inherits.inhparent = parent.oid
JOIN pg_class child ON pg_inherits.inhrelid = child.oid
WHERE parent.relname = 'binance_lob_ticks'
  AND child.relname >= 'binance_lob_ticks_20260329'
ORDER BY child.relname;
