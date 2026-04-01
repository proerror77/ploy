-- Fix Deribit IV partitions for April 2026
-- Create missing daily partitions

-- April 2026 partitions
CREATE TABLE IF NOT EXISTS deribit_iv_ticks_new_20260401
PARTITION OF deribit_iv_ticks
FOR VALUES FROM ('2026-04-01 00:00:00+08') TO ('2026-04-02 00:00:00+08');

CREATE TABLE IF NOT EXISTS deribit_iv_ticks_new_20260402
PARTITION OF deribit_iv_ticks
FOR VALUES FROM ('2026-04-02 00:00:00+08') TO ('2026-04-03 00:00:00+08');

CREATE TABLE IF NOT EXISTS deribit_iv_ticks_new_20260403
PARTITION OF deribit_iv_ticks
FOR VALUES FROM ('2026-04-03 00:00:00+08') TO ('2026-04-04 00:00:00+08');

CREATE TABLE IF NOT EXISTS deribit_iv_ticks_new_20260404
PARTITION OF deribit_iv_ticks
FOR VALUES FROM ('2026-04-04 00:00:00+08') TO ('2026-04-05 00:00:00+08');

CREATE TABLE IF NOT EXISTS deribit_iv_ticks_new_20260405
PARTITION OF deribit_iv_ticks
FOR VALUES FROM ('2026-04-05 00:00:00+08') TO ('2026-04-06 00:00:00+08');

CREATE TABLE IF NOT EXISTS deribit_iv_ticks_new_20260406
PARTITION OF deribit_iv_ticks
FOR VALUES FROM ('2026-04-06 00:00:00+08') TO ('2026-04-07 00:00:00+08');

CREATE TABLE IF NOT EXISTS deribit_iv_ticks_new_20260407
PARTITION OF deribit_iv_ticks
FOR VALUES FROM ('2026-04-07 00:00:00+08') TO ('2026-04-08 00:00:00+08');

-- Verify partitions
SELECT
    schemaname,
    tablename,
    pg_size_pretty(pg_total_relation_size(schemaname||'.'||tablename)) AS size
FROM pg_tables
WHERE tablename LIKE 'deribit_iv_ticks_new_202604%'
ORDER BY tablename;

-- Show partition info
SELECT
    child.relname AS partition_name,
    pg_get_expr(child.relpartbound, child.oid) AS partition_expression
FROM pg_inherits
JOIN pg_class parent ON pg_inherits.inhparent = parent.oid
JOIN pg_class child ON pg_inherits.inhrelid = child.oid
WHERE parent.relname = 'deribit_iv_ticks'
  AND child.relname LIKE '%202604%'
ORDER BY child.relname;
