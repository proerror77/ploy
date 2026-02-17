#!/usr/bin/env bash
set -euo pipefail

# Host + DB maintenance for always-on trading workloads.
#
# Goals:
# - keep the root disk from filling (logs, journals)
# - enforce retention on high-volume tables (ticks/observations)
#
# Intended to be run by systemd as root (see deployment/ploy-maintenance.*).

DB_NAME="${PLOY_DB_NAME:-ploy}"
LOG_DIR="${LOG_DIR:-/opt/ploy/logs}"

RETENTION_CLOB_TICKS_DAYS="${PLOY_RETENTION_CLOB_TICKS_DAYS:-7}"
RETENTION_CLOB_BOOK_DAYS="${PLOY_RETENTION_CLOB_BOOK_DAYS:-7}"
RETENTION_CLOB_ORDERBOOK_HISTORY_DAYS="${PLOY_RETENTION_CLOB_ORDERBOOK_HISTORY_DAYS:-7}"
RETENTION_CLOB_TRADES_DAYS="${PLOY_RETENTION_CLOB_TRADES_DAYS:-14}"
RETENTION_CLOB_ALERTS_DAYS="${PLOY_RETENTION_CLOB_ALERTS_DAYS:-30}"
RETENTION_BINANCE_TICKS_DAYS="${PLOY_RETENTION_BINANCE_TICKS_DAYS:-7}"
RETENTION_BINANCE_LOB_DAYS="${PLOY_RETENTION_BINANCE_LOB_DAYS:-7}"
RETENTION_NBA_OBS_DAYS="${PLOY_RETENTION_NBA_OBS_DAYS:-30}"
RETENTION_ORDER_EXEC_DAYS="${PLOY_RETENTION_ORDER_EXEC_DAYS:-30}"
RETENTION_LOG_DAYS="${PLOY_RETENTION_LOG_DAYS:-14}"
JOURNAL_VACUUM_SIZE="${PLOY_JOURNAL_VACUUM_SIZE:-200M}"

is_uint() {
  [[ "${1:-}" =~ ^[0-9]+$ ]]
}

if ! is_uint "$RETENTION_CLOB_TICKS_DAYS"; then
  echo "invalid PLOY_RETENTION_CLOB_TICKS_DAYS: $RETENTION_CLOB_TICKS_DAYS" >&2
  exit 2
fi
if ! is_uint "$RETENTION_CLOB_BOOK_DAYS"; then
  echo "invalid PLOY_RETENTION_CLOB_BOOK_DAYS: $RETENTION_CLOB_BOOK_DAYS" >&2
  exit 2
fi
if ! is_uint "$RETENTION_CLOB_ORDERBOOK_HISTORY_DAYS"; then
  echo "invalid PLOY_RETENTION_CLOB_ORDERBOOK_HISTORY_DAYS: $RETENTION_CLOB_ORDERBOOK_HISTORY_DAYS" >&2
  exit 2
fi
if ! is_uint "$RETENTION_CLOB_TRADES_DAYS"; then
  echo "invalid PLOY_RETENTION_CLOB_TRADES_DAYS: $RETENTION_CLOB_TRADES_DAYS" >&2
  exit 2
fi
if ! is_uint "$RETENTION_CLOB_ALERTS_DAYS"; then
  echo "invalid PLOY_RETENTION_CLOB_ALERTS_DAYS: $RETENTION_CLOB_ALERTS_DAYS" >&2
  exit 2
fi
if ! is_uint "$RETENTION_BINANCE_TICKS_DAYS"; then
  echo "invalid PLOY_RETENTION_BINANCE_TICKS_DAYS: $RETENTION_BINANCE_TICKS_DAYS" >&2
  exit 2
fi
if ! is_uint "$RETENTION_BINANCE_LOB_DAYS"; then
  echo "invalid PLOY_RETENTION_BINANCE_LOB_DAYS: $RETENTION_BINANCE_LOB_DAYS" >&2
  exit 2
fi
if ! is_uint "$RETENTION_NBA_OBS_DAYS"; then
  echo "invalid PLOY_RETENTION_NBA_OBS_DAYS: $RETENTION_NBA_OBS_DAYS" >&2
  exit 2
fi
if ! is_uint "$RETENTION_ORDER_EXEC_DAYS"; then
  echo "invalid PLOY_RETENTION_ORDER_EXEC_DAYS: $RETENTION_ORDER_EXEC_DAYS" >&2
  exit 2
fi
if ! is_uint "$RETENTION_LOG_DAYS"; then
  echo "invalid PLOY_RETENTION_LOG_DAYS: $RETENTION_LOG_DAYS" >&2
  exit 2
fi

echo "ploy_maintenance: db=${DB_NAME} log_dir=${LOG_DIR} clob_ticks_days=${RETENTION_CLOB_TICKS_DAYS} clob_book_days=${RETENTION_CLOB_BOOK_DAYS} clob_obh_days=${RETENTION_CLOB_ORDERBOOK_HISTORY_DAYS} clob_trades_days=${RETENTION_CLOB_TRADES_DAYS} clob_alerts_days=${RETENTION_CLOB_ALERTS_DAYS} binance_ticks_days=${RETENTION_BINANCE_TICKS_DAYS} binance_lob_days=${RETENTION_BINANCE_LOB_DAYS} nba_obs_days=${RETENTION_NBA_OBS_DAYS} order_exec_days=${RETENTION_ORDER_EXEC_DAYS} log_days=${RETENTION_LOG_DAYS}"

if command -v runuser >/dev/null 2>&1; then
  PSQL=(runuser -u postgres -- psql -d "$DB_NAME" -v ON_ERROR_STOP=1)
else
  # Fallback for minimal distros.
  PSQL=(su -s /bin/bash postgres -c "psql -d \"$DB_NAME\" -v ON_ERROR_STOP=1")
fi

echo "==> DB retention"
"${PSQL[@]}" <<SQL
-- High-volume tick table
DELETE FROM clob_quote_ticks
WHERE received_at < NOW() - INTERVAL '${RETENTION_CLOB_TICKS_DAYS} days';

-- CLOB order book snapshots (optional; table may not exist on all hosts)
SELECT format(
  'DELETE FROM clob_orderbook_snapshots WHERE received_at < NOW() - INTERVAL ''%s days'';',
  ${RETENTION_CLOB_BOOK_DAYS}
) WHERE to_regclass('public.clob_orderbook_snapshots') IS NOT NULL \\gexec

-- CLOB orderbook-history L2 ticks (optional; table may not exist on all hosts)
SELECT format(
  'DELETE FROM clob_orderbook_history_ticks WHERE book_ts < NOW() - INTERVAL ''%s days'';',
  ${RETENTION_CLOB_ORDERBOOK_HISTORY_DAYS}
) WHERE to_regclass('public.clob_orderbook_history_ticks') IS NOT NULL \\gexec

-- CLOB trade ticks (optional; table may not exist on all hosts)
SELECT format(
  'DELETE FROM clob_trade_ticks WHERE received_at < NOW() - INTERVAL ''%s days'';',
  ${RETENTION_CLOB_TRADES_DAYS}
) WHERE to_regclass('public.clob_trade_ticks') IS NOT NULL \\gexec

-- Trade alerts (optional; table may not exist on all hosts)
SELECT format(
  'DELETE FROM clob_trade_alerts WHERE created_at < NOW() - INTERVAL ''%s days'';',
  ${RETENTION_CLOB_ALERTS_DAYS}
) WHERE to_regclass('public.clob_trade_alerts') IS NOT NULL \\gexec

-- Binance spot price ticks (optional; table may not exist on all hosts)
SELECT format(
  'DELETE FROM binance_price_ticks WHERE received_at < NOW() - INTERVAL ''%s days'';',
  ${RETENTION_BINANCE_TICKS_DAYS}
) WHERE to_regclass('public.binance_price_ticks') IS NOT NULL \\gexec

-- Binance LOB ticks (optional; table may not exist on all hosts)
SELECT format(
  'DELETE FROM binance_lob_ticks WHERE event_time < NOW() - INTERVAL ''%s days'';',
  ${RETENTION_BINANCE_LOB_DAYS}
) WHERE to_regclass('public.binance_lob_ticks') IS NOT NULL \\gexec

-- Sports observations (moderate volume)
DELETE FROM nba_live_observations
WHERE recorded_at < NOW() - INTERVAL '${RETENTION_NBA_OBS_DAYS} days';

-- Agent execution records (optional; table may not exist on all hosts)
SELECT format(
  'DELETE FROM agent_order_executions WHERE executed_at < NOW() - INTERVAL ''%s days'';',
  ${RETENTION_ORDER_EXEC_DAYS}
) WHERE to_regclass('public.agent_order_executions') IS NOT NULL \\gexec

VACUUM (ANALYZE) clob_quote_ticks;
SELECT 'VACUUM (ANALYZE) clob_orderbook_snapshots;' WHERE to_regclass('public.clob_orderbook_snapshots') IS NOT NULL \\gexec
SELECT 'VACUUM (ANALYZE) clob_orderbook_history_ticks;' WHERE to_regclass('public.clob_orderbook_history_ticks') IS NOT NULL \\gexec
SELECT 'VACUUM (ANALYZE) clob_trade_ticks;' WHERE to_regclass('public.clob_trade_ticks') IS NOT NULL \\gexec
SELECT 'VACUUM (ANALYZE) clob_trade_alerts;' WHERE to_regclass('public.clob_trade_alerts') IS NOT NULL \\gexec
SELECT 'VACUUM (ANALYZE) binance_price_ticks;' WHERE to_regclass('public.binance_price_ticks') IS NOT NULL \\gexec
SELECT 'VACUUM (ANALYZE) binance_lob_ticks;' WHERE to_regclass('public.binance_lob_ticks') IS NOT NULL \\gexec
VACUUM (ANALYZE) nba_live_observations;
SELECT 'VACUUM (ANALYZE) agent_order_executions;' WHERE to_regclass('public.agent_order_executions') IS NOT NULL \\gexec
SQL

echo "==> Log retention"
if [[ -d "$LOG_DIR" ]]; then
  # Compress older uncompressed logs.
  find "$LOG_DIR" -maxdepth 1 -type f -name 'ploy.log.*' -mtime +1 ! -name '*.gz' -print0 \
    | xargs -0 -r gzip -9

  # Delete old compressed logs.
  find "$LOG_DIR" -maxdepth 1 -type f -name 'ploy.log.*.gz' -mtime +"$RETENTION_LOG_DAYS" -delete
fi

echo "==> Journald vacuum"
if command -v journalctl >/dev/null 2>&1; then
  journalctl --vacuum-size="$JOURNAL_VACUUM_SIZE" >/dev/null 2>&1 || true
fi

echo "ploy_maintenance: done"
