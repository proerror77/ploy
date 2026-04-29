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
EXTRA_LOG_DIRS="${PLOY_EXTRA_LOG_DIRS:-/var/log/ploy}"

RETENTION_CLOB_TICKS_DAYS="${PLOY_RETENTION_CLOB_TICKS_DAYS:-7}"
RETENTION_CLOB_BOOK_DAYS="${PLOY_RETENTION_CLOB_BOOK_DAYS:-7}"
RETENTION_CLOB_ORDERBOOK_HISTORY_DAYS="${PLOY_RETENTION_CLOB_ORDERBOOK_HISTORY_DAYS:-7}"
RETENTION_CLOB_TRADES_DAYS="${PLOY_RETENTION_CLOB_TRADES_DAYS:-7}"
RETENTION_CLOB_ALERTS_DAYS="${PLOY_RETENTION_CLOB_ALERTS_DAYS:-7}"
RETENTION_BINANCE_TICKS_DAYS="${PLOY_RETENTION_BINANCE_TICKS_DAYS:-7}"
RETENTION_BINANCE_AGGTRADE_DAYS="${PLOY_RETENTION_BINANCE_AGGTRADE_DAYS:-7}"
RETENTION_BINANCE_LOB_DAYS="${PLOY_RETENTION_BINANCE_LOB_DAYS:-7}"
RETENTION_NBA_OBS_DAYS="${PLOY_RETENTION_NBA_OBS_DAYS:-7}"
RETENTION_ORDER_EXEC_DAYS="${PLOY_RETENTION_ORDER_EXEC_DAYS:-7}"
RETENTION_LOG_DAYS="${PLOY_RETENTION_LOG_DAYS:-14}"
RETENTION_LOG_MAX_FILE_MB="${PLOY_RETENTION_LOG_MAX_FILE_MB:-512}"
RETENTION_LOG_MAX_DIR_MB="${PLOY_RETENTION_LOG_MAX_DIR_MB:-1024}"
JOURNAL_VACUUM_SIZE="${PLOY_JOURNAL_VACUUM_SIZE:-200M}"
DERIBIT_PARTITION_LOOKBACK_DAYS="${PLOY_DERIBIT_PARTITION_LOOKBACK_DAYS:-7}"
DERIBIT_PARTITION_LOOKAHEAD_DAYS="${PLOY_DERIBIT_PARTITION_LOOKAHEAD_DAYS:-14}"
REFRESH_RESEARCH_WINDOWS="${PLOY_REFRESH_RESEARCH_WINDOWS:-false}"
LOGS_ONLY="${PLOY_MAINTENANCE_LOGS_ONLY:-false}"

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
if ! is_uint "$RETENTION_BINANCE_AGGTRADE_DAYS"; then
  echo "invalid PLOY_RETENTION_BINANCE_AGGTRADE_DAYS: $RETENTION_BINANCE_AGGTRADE_DAYS" >&2
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
if ! is_uint "$RETENTION_LOG_MAX_FILE_MB"; then
  echo "invalid PLOY_RETENTION_LOG_MAX_FILE_MB: $RETENTION_LOG_MAX_FILE_MB" >&2
  exit 2
fi
if ! is_uint "$RETENTION_LOG_MAX_DIR_MB"; then
  echo "invalid PLOY_RETENTION_LOG_MAX_DIR_MB: $RETENTION_LOG_MAX_DIR_MB" >&2
  exit 2
fi
if ! is_uint "$DERIBIT_PARTITION_LOOKBACK_DAYS"; then
  echo "invalid PLOY_DERIBIT_PARTITION_LOOKBACK_DAYS: $DERIBIT_PARTITION_LOOKBACK_DAYS" >&2
  exit 2
fi
if ! is_uint "$DERIBIT_PARTITION_LOOKAHEAD_DAYS"; then
  echo "invalid PLOY_DERIBIT_PARTITION_LOOKAHEAD_DAYS: $DERIBIT_PARTITION_LOOKAHEAD_DAYS" >&2
  exit 2
fi

echo "ploy_maintenance: db=${DB_NAME} log_dir=${LOG_DIR} extra_log_dirs=${EXTRA_LOG_DIRS} clob_ticks_days=${RETENTION_CLOB_TICKS_DAYS} clob_book_days=${RETENTION_CLOB_BOOK_DAYS} clob_obh_days=${RETENTION_CLOB_ORDERBOOK_HISTORY_DAYS} clob_trades_days=${RETENTION_CLOB_TRADES_DAYS} clob_alerts_days=${RETENTION_CLOB_ALERTS_DAYS} binance_ticks_days=${RETENTION_BINANCE_TICKS_DAYS} binance_aggtrade_days=${RETENTION_BINANCE_AGGTRADE_DAYS} binance_lob_days=${RETENTION_BINANCE_LOB_DAYS} nba_obs_days=${RETENTION_NBA_OBS_DAYS} order_exec_days=${RETENTION_ORDER_EXEC_DAYS} log_days=${RETENTION_LOG_DAYS} log_max_file_mb=${RETENTION_LOG_MAX_FILE_MB} log_max_dir_mb=${RETENTION_LOG_MAX_DIR_MB} deribit_partition_lookback_days=${DERIBIT_PARTITION_LOOKBACK_DAYS} deribit_partition_lookahead_days=${DERIBIT_PARTITION_LOOKAHEAD_DAYS} refresh_research_windows=${REFRESH_RESEARCH_WINDOWS} logs_only=${LOGS_ONLY}"

prune_log_dir() {
  local dir="$1"
  local current_mb oldest
  local timestamp size path

  [[ -d "$dir" ]] || return 0

  echo "Pruning logs in ${dir}"

  # Delete old log files before compression so stale giant files disappear in one run.
  find "$dir" -maxdepth 1 -type f -mtime +"$RETENTION_LOG_DAYS" \
    \( -name '*.log' -o -name '*.log.*' -o -name '*.log.*.gz' -o -name 'ploy.log*' -o -name 'platform.log*' \) \
    -delete

  # Delete old rotated files that are too large even if logrotate touched mtime recently.
  find "$dir" -maxdepth 1 -type f -mtime +1 -size +"${RETENTION_LOG_MAX_FILE_MB}"M \
    \( -name '*.log.*' -o -name '*.log.*.gz' -o -name 'ploy.log.*' -o -name 'platform.log.*' \) \
    -delete

  # Compress older uncompressed logs that survived deletion.
  find "$dir" -maxdepth 1 -type f -mtime +1 ! -name '*.gz' \
    \( -name '*.log' -o -name '*.log.*' -o -name 'ploy.log*' -o -name 'platform.log*' \) \
    -print0 | xargs -0 -r gzip -9

  # Delete old compressed logs.
  find "$dir" -maxdepth 1 -type f -name '*.gz' -mtime +"$RETENTION_LOG_DAYS" -delete

  current_mb=$(du -sm "$dir" | awk '{print $1}')
  if (( current_mb <= RETENTION_LOG_MAX_DIR_MB )); then
    echo "Log dir ${dir} is ${current_mb}M, under ${RETENTION_LOG_MAX_DIR_MB}M cap"
    return 0
  fi

  echo "Log dir ${dir} is ${current_mb}M, pruning largest files to ${RETENTION_LOG_MAX_DIR_MB}M cap"
  while IFS= read -r -d '' oldest; do
    size="${oldest%% *}"
    oldest="${oldest#* }"
    timestamp="${oldest%% *}"
    path="${oldest#* }"
    echo "Deleting ${path} (${size} bytes, ts=${timestamp})"
    rm -f -- "$path"
    current_mb=$(du -sm "$dir" | awk '{print $1}')
    (( current_mb <= RETENTION_LOG_MAX_DIR_MB )) && break
  done < <(
    find "$dir" -maxdepth 1 -type f \
      \( -name '*.log' -o -name '*.log.*' -o -name '*.log.*.gz' -o -name 'ploy.log*' -o -name 'platform.log*' \) \
      -printf '%s %T@ %p\0' | sort -z -nr
  )
  echo "Log dir ${dir} is now $(du -sm "$dir" | awk '{print $1}')M"
}

if [[ "$LOGS_ONLY" != "true" ]]; then
  if [[ -n "${DATABASE_URL:-}" ]]; then
    PSQL=(psql "$DATABASE_URL" -v ON_ERROR_STOP=1)
  elif command -v runuser >/dev/null 2>&1; then
    PSQL=(runuser -u postgres -- psql -d "$DB_NAME" -v ON_ERROR_STOP=1)
  else
    # Fallback for minimal distros.
    PSQL=(su -s /bin/bash postgres -c "psql -d \"$DB_NAME\" -v ON_ERROR_STOP=1")
  fi

  echo "==> DB retention"
  if [[ "$REFRESH_RESEARCH_WINDOWS" == "true" ]]; then
    "${PSQL[@]}" <<SQL
-- Refresh research valid windows materialized view (if it exists).
-- CONCURRENTLY avoids locking reads during refresh; requires the UNIQUE index.
SELECT 'REFRESH MATERIALIZED VIEW CONCURRENTLY research_valid_windows;'
WHERE to_regclass('public.research_valid_windows') IS NOT NULL \\gexec
SQL
  else
    echo "Skipping research_valid_windows refresh; set PLOY_REFRESH_RESEARCH_WINDOWS=true to enable."
  fi

  "${PSQL[@]}" <<SQL

DO \$\$
DECLARE
  partition_day date := current_date - ${DERIBIT_PARTITION_LOOKBACK_DAYS};
  end_day date := current_date + ${DERIBIT_PARTITION_LOOKAHEAD_DAYS};
  partition_name text;
BEGIN
  IF to_regclass('public.deribit_iv_ticks') IS NOT NULL THEN
    WHILE partition_day <= end_day LOOP
      partition_name := format('deribit_iv_ticks_new_%s', to_char(partition_day, 'YYYYMMDD'));
      BEGIN
        EXECUTE format(
          'CREATE TABLE IF NOT EXISTS %I PARTITION OF deribit_iv_ticks FOR VALUES FROM (%L) TO (%L);',
          partition_name,
          format('%s 00:00:00+08', partition_day),
          format('%s 00:00:00+08', partition_day + 1)
        );
      EXCEPTION
        WHEN duplicate_table THEN
          NULL;
        WHEN OTHERS THEN
          IF position('would overlap partition' in SQLERRM) > 0 THEN
            NULL;
          ELSE
            RAISE;
          END IF;
      END;
      partition_day := partition_day + 1;
    END LOOP;
  END IF;
END
\$\$;

DO \$\$
DECLARE
  partition_day date := current_date - ${DERIBIT_PARTITION_LOOKBACK_DAYS};
  end_day date := current_date + ${DERIBIT_PARTITION_LOOKAHEAD_DAYS};
  partition_name text;
BEGIN
  IF to_regclass('public.binance_lob_ticks') IS NOT NULL THEN
    WHILE partition_day <= end_day LOOP
      partition_name := format('binance_lob_ticks_new_%s', to_char(partition_day, 'YYYYMMDD'));
      BEGIN
        EXECUTE format(
          'CREATE TABLE IF NOT EXISTS %I PARTITION OF binance_lob_ticks FOR VALUES FROM (%L) TO (%L);',
          partition_name,
          format('%s 00:00:00+08', partition_day),
          format('%s 00:00:00+08', partition_day + 1)
        );
      EXCEPTION
        WHEN duplicate_table THEN
          NULL;
        WHEN OTHERS THEN
          IF position('would overlap partition' in SQLERRM) > 0 THEN
            NULL;
          ELSE
            RAISE;
          END IF;
      END;
      partition_day := partition_day + 1;
    END LOOP;
  END IF;
END
\$\$;

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

-- Binance aggTrade ticks (optional; table may not exist on all hosts)
SELECT format(
  'DELETE FROM binance_agg_trade_ticks WHERE received_at < NOW() - INTERVAL ''%s days'';',
  ${RETENTION_BINANCE_AGGTRADE_DAYS}
) WHERE to_regclass('public.binance_agg_trade_ticks') IS NOT NULL \\gexec

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
SELECT 'VACUUM (ANALYZE) binance_agg_trade_ticks;' WHERE to_regclass('public.binance_agg_trade_ticks') IS NOT NULL \\gexec
SELECT 'VACUUM (ANALYZE) binance_lob_ticks;' WHERE to_regclass('public.binance_lob_ticks') IS NOT NULL \\gexec
VACUUM (ANALYZE) nba_live_observations;
SELECT 'VACUUM (ANALYZE) agent_order_executions;' WHERE to_regclass('public.agent_order_executions') IS NOT NULL \\gexec
SQL
else
  echo "Skipping DB retention because PLOY_MAINTENANCE_LOGS_ONLY=true"
fi

echo "==> Log retention"
seen_log_dirs=" "
for dir in "$LOG_DIR" $EXTRA_LOG_DIRS; do
  [[ -n "${dir:-}" ]] || continue
  case "$seen_log_dirs" in
    *" $dir "*) continue ;;
  esac
  seen_log_dirs="${seen_log_dirs}${dir} "
  prune_log_dir "$dir"
done

echo "==> Journald vacuum"
if command -v journalctl >/dev/null 2>&1; then
  journalctl --vacuum-size="$JOURNAL_VACUUM_SIZE" >/dev/null 2>&1 || true
fi

echo "ploy_maintenance: done"
